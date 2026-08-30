//! Resolve captured HID++ inputs against the active per-device plan.

mod momentum;
mod wheel;

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::touchpad::{GestureRecognition, TouchFrame, TouchpadGestureRecognizer};
use openlogi_hid::CapturedInput;
use openlogi_inject::SmoothScrollPhase;
use tracing::debug;

use self::momentum::TouchpadMomentum;
use self::wheel::{ScrollScale, WheelAccumulators, WheelOutput, WheelRotation};
use super::{GestureOutputs, TouchpadScrollTuning};
use crate::capture_plan::DispatchPlan;
use crate::runtime::hook::SharedHookMaps;
use crate::runtime::{HidppSessionId, PressToken};

/// Effective thumb-wheel configuration whose continuity is tied to one
/// dispatch plan. A binding or sensitivity update clears accumulated state
/// without cycling an unchanged HID++ diversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WheelConfiguration {
    up: Action,
    down: Action,
    sensitivity: ThumbwheelSensitivity,
}

impl WheelConfiguration {
    /// Resolve both directional bindings and their shared sensitivity.
    pub(super) fn for_plan(plan: &DispatchPlan) -> Self {
        let action = |button| {
            plan.bindings
                .get(&button)
                .map_or_else(|| default_binding(button), Binding::click_action)
        };
        Self {
            up: action(ButtonId::ThumbwheelScrollUp),
            down: action(ButtonId::ThumbwheelScrollDown),
            sensitivity: plan.thumbwheel_sensitivity,
        }
    }

    fn action(&self, rotation: WheelRotation) -> &Action {
        match rotation.button() {
            ButtonId::ThumbwheelScrollUp => &self.up,
            ButtonId::ThumbwheelScrollDown => &self.down,
            _ => unreachable!("wheel rotations only map to thumb-wheel directions"),
        }
    }
}

/// Correlates completed HID++ gesture semantics with the exact physical press
/// token admitted by the shared button runtime. The runtime remains the sole
/// authority on whether the token is still active.
#[derive(Default)]
struct GesturePresses {
    tokens: HashMap<(HidppSessionId, ButtonId), PressToken>,
}

impl GesturePresses {
    fn start(&mut self, session: &HidppSessionId, button: ButtonId, press: PressToken) {
        self.tokens.insert((session.clone(), button), press);
    }

    fn get(&self, session: &HidppSessionId, button: ButtonId) -> Option<&PressToken> {
        self.tokens.get(&(session.clone(), button))
    }

    fn end(&mut self, session: &HidppSessionId, button: ButtonId) {
        self.tokens.remove(&(session.clone(), button));
    }

    fn cancel_session(&mut self, session: &HidppSessionId) {
        self.tokens.retain(|(candidate, _), _| candidate != session);
    }
}

/// Wheel state scoped to exact capture-session incarnations. Keying by session
/// rather than device prevents a replacement epoch from inheriting progress or
/// having its state removed by a stale completion from the previous epoch.
#[derive(Default)]
struct SessionWheels(HashMap<HidppSessionId, WheelAccumulators>);

impl SessionWheels {
    fn for_session(&mut self, session: &HidppSessionId) -> &mut WheelAccumulators {
        self.0.entry(session.clone()).or_default()
    }

    fn cancel_session(&mut self, session: &HidppSessionId) {
        self.0.remove(session);
    }
}

/// One routed outcome of feeding a frame (or a stroke boundary) to a
/// session's touchpad runtime.
#[derive(Debug, PartialEq)]
enum TouchpadOutput {
    /// Nothing to dispatch for this frame.
    Idle,
    /// A committed gesture trigger with its resolved action.
    Action { trigger: ButtonId, action: Action },
    /// One synthesized two-finger scroll frame: the centroid's travel in
    /// micrometres plus its position in the scroll gesture's phase stream.
    Scroll {
        dx_um: i64,
        dy_um: i64,
        phase: SmoothScrollPhase,
    },
    /// The scroll stream closed. The zero-delta phase event is posted by the
    /// dispatcher; `exit_velocity` (centroid micrometres per second, tracked
    /// as an exponential average over the stroke) seeds momentum on a clean
    /// end and is absent on a cancellation.
    ScrollEnd {
        phase: SmoothScrollPhase,
        exit_velocity_um_per_s: Option<(f64, f64)>,
    },
}

#[derive(Default)]
struct TouchpadRuntime {
    recognizer: TouchpadGestureRecognizer,
    frozen_bindings: Option<BTreeMap<ButtonId, Action>>,
    frozen_actions_enabled: bool,
    /// Whether this stroke already opened its scroll stream, so the next
    /// delta knows to continue rather than begin one.
    scroll_streaming: bool,
    /// Smoothed centroid velocity over the stroke, micrometres per second:
    /// fast-attack, slow-release (α = 0.01 per frame), the filter the
    /// Options+ agent feeds its inertia with — a brief slowdown before
    /// lift-off must not kill a glide.
    scroll_velocity_um_per_s: (f64, f64),
    /// Timestamp of the last frame seen this stroke, the dt baseline for the
    /// velocity filter — recorded on every frame, so the first streamed
    /// delta (which already spans one frame of travel) gets a real dt too.
    last_frame_us: Option<u64>,
}

impl TouchpadRuntime {
    fn update(
        &mut self,
        frame: &TouchFrame,
        current_bindings: &BTreeMap<ButtonId, Action>,
        actions_enabled: bool,
    ) -> TouchpadOutput {
        if self.frozen_bindings.is_none() {
            self.frozen_bindings = Some(current_bindings.clone());
            self.frozen_actions_enabled = actions_enabled;
        }
        let timestamp_us = frame.timestamp_us;
        #[expect(
            clippy::cast_precision_loss,
            reason = "frame timestamps stay far below 2^53 microseconds"
        )]
        let dt_us = self.last_frame_us.map_or(f64::NAN, |previous| {
            (timestamp_us.saturating_sub(previous)) as f64
        });
        self.last_frame_us = Some(timestamp_us);
        match self.recognizer.update(frame) {
            GestureRecognition::Gesture(trigger)
                if self.frozen_actions_enabled && actions_enabled =>
            {
                self.action(trigger)
                    .map_or(TouchpadOutput::Idle, |(trigger, action)| {
                        TouchpadOutput::Action { trigger, action }
                    })
            }
            GestureRecognition::Scroll { dx_um, dy_um } => {
                // Scrolling replaces the firmware translation the capture
                // switched off, so it flows regardless of action bindings.
                let phase = if self.scroll_streaming {
                    SmoothScrollPhase::Changed
                } else {
                    SmoothScrollPhase::Began
                };
                self.scroll_streaming = true;
                self.track_scroll_velocity(dt_us, dx_um, dy_um);
                TouchpadOutput::Scroll {
                    dx_um,
                    dy_um,
                    phase,
                }
            }
            GestureRecognition::Pending | GestureRecognition::Gesture(_) => TouchpadOutput::Idle,
        }
    }

    fn end(&mut self, actions_enabled: bool) -> TouchpadOutput {
        let action = self
            .recognizer
            .end()
            .filter(|_| self.frozen_actions_enabled && actions_enabled)
            .and_then(|trigger| self.action(trigger));
        let terminal = self.close_scroll_stream(SmoothScrollPhase::Ended);
        self.frozen_bindings = None;
        self.frozen_actions_enabled = false;
        terminal.unwrap_or_else(|| {
            action.map_or(TouchpadOutput::Idle, |(trigger, action)| {
                TouchpadOutput::Action { trigger, action }
            })
        })
    }

    fn cancel(&mut self) -> TouchpadOutput {
        let terminal = self.close_scroll_stream(SmoothScrollPhase::Cancelled);
        self.recognizer.cancel();
        self.frozen_bindings = None;
        self.frozen_actions_enabled = false;
        terminal.unwrap_or(TouchpadOutput::Idle)
    }

    /// Terminate an open scroll stream, if any. Scrolling travelled past the
    /// tap limits, so a scrolled stroke can never also resolve a tap and the
    /// two outcomes never compete. A cancellation discards the velocity —
    /// momentum must not grow out of a stroke the stream rejected.
    fn close_scroll_stream(&mut self, phase: SmoothScrollPhase) -> Option<TouchpadOutput> {
        let velocity = self.scroll_velocity_um_per_s;
        self.scroll_velocity_um_per_s = (0.0, 0.0);
        self.last_frame_us = None;
        self.scroll_streaming.then(|| {
            self.scroll_streaming = false;
            TouchpadOutput::ScrollEnd {
                phase,
                exit_velocity_um_per_s: (phase == SmoothScrollPhase::Ended).then_some(velocity),
            }
        })
    }

    /// Fold one frame's delta into the exit-velocity filter.
    #[expect(
        clippy::cast_precision_loss,
        reason = "per-frame deltas stay far below 2^53 micrometres"
    )]
    fn track_scroll_velocity(&mut self, dt_us: f64, dx: i64, dy: i64) {
        const RELEASE: f64 = 0.99;
        const ATTACK: f64 = 0.01;
        if !dt_us.is_finite() || dt_us <= 0.0 {
            return;
        }
        let seconds = dt_us / 1_000_000.0;
        let raw = (dx as f64 / seconds, dy as f64 / seconds);
        let smoothed = &mut self.scroll_velocity_um_per_s;
        smoothed.0 = if raw.0.abs() > smoothed.0.abs() {
            raw.0
        } else {
            smoothed.0 * RELEASE + raw.0 * ATTACK
        };
        smoothed.1 = if raw.1.abs() > smoothed.1.abs() {
            raw.1
        } else {
            smoothed.1 * RELEASE + raw.1 * ATTACK
        };
    }

    fn action(&self, trigger: ButtonId) -> Option<(ButtonId, Action)> {
        self.frozen_bindings
            .as_ref()?
            .get(&trigger)
            .cloned()
            .map(|action| (trigger, action))
    }
}

#[derive(Default)]
struct SessionTouchpads(HashMap<HidppSessionId, TouchpadRuntime>);

impl SessionTouchpads {
    fn for_session(&mut self, session: &HidppSessionId) -> &mut TouchpadRuntime {
        self.0.entry(session.clone()).or_default()
    }

    fn cancel_session(&mut self, session: &HidppSessionId) -> Option<TouchpadOutput> {
        self.0.remove(session).map(|mut runtime| runtime.cancel())
    }
}

/// Input routing plus the per-session state retained between
/// captured events. Capture-session lifecycle remains owned by the parent.
pub(super) struct InputDispatcher {
    hook_maps: SharedHookMaps,
    outputs: GestureOutputs,
    wheels: SessionWheels,
    gesture_presses: GesturePresses,
    touchpads: SessionTouchpads,
    /// The one running scroll-momentum tail. A new touch replaces it.
    momentum: Option<TouchpadMomentum>,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            gesture_presses: GesturePresses::default(),
            touchpads: SessionTouchpads::default(),
            momentum: None,
        }
    }

    /// Kill the scroll-momentum tail, if one is gliding. The thread posts its
    /// own terminal zero-delta end on the next tick, keeping the momentum
    /// phase machine single-ownered.
    fn stop_momentum(&mut self) {
        if let Some(momentum) = self.momentum.take() {
            momentum.stop();
        }
    }

    /// Publish a hardware polarity observation into the OS-hook snapshot.
    fn record_thumbwheel_direction(&self, key: &str, input: &CapturedInput) -> bool {
        let CapturedInput::ThumbwheelDirection {
            positive_is_forward,
        } = input
        else {
            return false;
        };
        if let Ok(mut maps) = self.hook_maps.write() {
            maps.thumbwheel_positive_is_forward
                .insert(key.to_owned(), *positive_is_forward);
        }
        true
    }

    /// Cancel every input lifecycle retained for one capture session.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) {
        self.stop_momentum();
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.gesture_presses.cancel_session(session);
        if let Some(outcome) = self.touchpads.cancel_session(session) {
            Self::route_touchpad_output(
                &self.outputs,
                TouchpadScrollTuning::NEUTRAL,
                session.device_key(),
                outcome,
            );
        }
    }

    /// Route one captured input from `session` to its bound action or
    /// re-synthesised scroll output.
    pub(super) fn dispatch(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        input: CapturedInput,
        touchpad_actions_enabled: bool,
    ) {
        let key = session.device_key();
        if self.record_thumbwheel_direction(key, &input) {
            return;
        }
        match input {
            CapturedInput::Gesture(button, direction) => {
                Self::dispatch_gesture(
                    &self.gesture_presses,
                    &self.outputs,
                    session,
                    plan,
                    button,
                    direction,
                );
            }
            CapturedInput::ButtonDown(button) => {
                // A raw-XY gesture source owns its click/swipe map; its physical
                // lifecycle is still tracked, but it must not also fire the
                // single-action projection on down.
                let is_gesture = plan.gesture_bindings.contains_key(&button)
                    || plan.side_gesture_bindings.contains_key(&button);
                let binding = (!is_gesture).then(|| plan.bindings.get(&button)).flatten();
                if let Some(binding) = binding {
                    debug!(key, ?button, action = %binding.click_action().label(), "HID++ button → binding");
                } else {
                    debug!(key, ?button, "HID++ button with no binding — ignored");
                }
                let press = self
                    .outputs
                    .actions
                    .try_hidpp_button_down(session, button, binding);
                if is_gesture {
                    if let Some(press) = press {
                        self.gesture_presses.start(session, button, press);
                    } else {
                        self.gesture_presses.end(session, button);
                    }
                }
            }
            CapturedInput::ButtonUp(button) => {
                self.outputs.actions.try_hidpp_button_up(session, button);
                self.gesture_presses.end(session, button);
            }
            CapturedInput::ButtonPulse(button) => {
                Self::dispatch_button_pulse(&self.outputs, session, plan, button);
            }
            CapturedInput::Scroll {
                increments,
                resolution,
            } => {
                let Some(rotation) = WheelRotation::from_increments(increments) else {
                    return;
                };
                let button = rotation.button();
                let configuration = WheelConfiguration::for_plan(plan);
                let action = configuration.action(rotation);
                let wheels = self.wheels.for_session(session);
                match wheels.advance(
                    rotation,
                    action,
                    ScrollScale::new(resolution, configuration.sensitivity),
                    Instant::now(),
                ) {
                    WheelOutput::Idle => {}
                    WheelOutput::Scroll(delta) => self.outputs.post_scroll(session, delta),
                    WheelOutput::FireAction => {
                        debug!(key, ?button, action = %action.label(), "thumb wheel → action");
                        self.outputs.actions.dispatch(action, Some(key));
                    }
                }
            }
            CapturedInput::ThumbwheelDirection { .. } => {
                unreachable!("thumb-wheel direction reports return before dispatch")
            }
            CapturedInput::TouchpadFrame(frame) => {
                // Touch re-lands: stop any gliding tail before the new stroke.
                self.stop_momentum();
                let tuning = TouchpadScrollTuning::from_plan(plan);
                let outcome = self.touchpads.for_session(session).update(
                    &frame,
                    &plan.touchpad_bindings,
                    touchpad_actions_enabled,
                );
                Self::route_touchpad_output(&self.outputs, tuning, key, outcome);
            }
            CapturedInput::TouchpadEnd => {
                self.end_touchpad_stroke(session, plan, key, touchpad_actions_enabled);
            }
            CapturedInput::TouchpadCancel => {
                self.stop_momentum();
                let tuning = TouchpadScrollTuning::from_plan(plan);
                let outcome = self.touchpads.for_session(session).cancel();
                Self::route_touchpad_output(&self.outputs, tuning, key, outcome);
            }
            CapturedInput::TouchpadDroppedFrames(_) => {}
        }
    }

    fn dispatch_gesture(
        gesture_presses: &GesturePresses,
        outputs: &GestureOutputs,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
        direction: GestureDirection,
    ) {
        let key = session.device_key();
        let Some(press) = gesture_presses.get(session, button) else {
            debug!(key, %button, ?direction, "gesture from a canceled button lifecycle — ignored");
            return;
        };
        let Some(action) = plan
            .gesture_bindings
            .get(&button)
            .or_else(|| plan.side_gesture_bindings.get(&button))
            .and_then(|map| map.get(&direction))
        else {
            debug!(key, %button, ?direction, "gesture with no binding — ignored");
            return;
        };
        debug!(key, %button, ?direction, action = %action.label(), "gesture → action");
        if !outputs.actions.try_dispatch_while_pressed(press, action) {
            debug!(key, %button, ?direction, "gesture press no longer active — ignored");
        }
    }

    fn dispatch_button_pulse(
        outputs: &GestureOutputs,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
    ) {
        let key = session.device_key();
        let binding = plan.bindings.get(&button);
        if let Some(binding) = binding {
            debug!(key, ?button, action = %binding.click_action().label(), "HID++ button pulse → binding");
        } else {
            debug!(key, ?button, "HID++ button pulse with no binding — ignored");
        }
        outputs
            .actions
            .dispatch_hidpp_button_pulse(session, button, binding);
    }

    /// End one touchpad stroke: route the terminal (zero-delta phase event,
    /// or a tap that survived the scroll travel limits) and, when the
    /// lift-off was fast enough, hand the exit velocity to a momentum tail.
    /// The tail is wheel-class output, so the gesture stream's own terminal
    /// still closes it and the two never share a phase machine.
    fn end_touchpad_stroke(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        key: &str,
        actions_enabled: bool,
    ) {
        let tuning = TouchpadScrollTuning::from_plan(plan);
        let outcome = self.touchpads.for_session(session).end(actions_enabled);
        let exit_velocity = match &outcome {
            TouchpadOutput::ScrollEnd {
                exit_velocity_um_per_s,
                ..
            } => *exit_velocity_um_per_s,
            _ => None,
        };
        Self::route_touchpad_output(&self.outputs, tuning, key, outcome);
        if let Some(exit_velocity) = exit_velocity {
            self.momentum = TouchpadMomentum::start(tuning, exit_velocity);
        }
    }

    fn route_touchpad_output(
        outputs: &GestureOutputs,
        tuning: TouchpadScrollTuning,
        key: &str,
        outcome: TouchpadOutput,
    ) {
        match outcome {
            TouchpadOutput::Idle => {}
            TouchpadOutput::Action { trigger, action } => {
                debug!(key, %trigger, action = %action.label(), "touchpad gesture → action");
                outputs.actions.dispatch(&action, Some(key));
            }
            TouchpadOutput::Scroll {
                dx_um,
                dy_um,
                phase,
            } => super::post_touchpad_scroll(tuning, dx_um, dy_um, phase),
            TouchpadOutput::ScrollEnd { phase, .. } => {
                // Zero distance: the phase transition itself closes the
                // gesture stream; momentum, if any, is the caller's call.
                super::post_touchpad_scroll(tuning, 0, 0, phase);
            }
        }
    }
}

#[cfg(test)]
mod tests;
