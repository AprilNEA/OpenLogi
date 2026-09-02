//! Resolve captured HID++ inputs against the active per-device plan.

mod momentum;
mod swipe;
mod wheel;

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::touchpad::{GestureRecognition, TouchFrame, TouchpadGestureRecognizer};
use openlogi_hid::CapturedInput;
use openlogi_hid::thumbwheel::WheelResolution;
use tracing::debug;

use self::momentum::TouchpadMomentum;
use self::swipe::{ActiveSwipe, SpreadBank, SwipeEnd, SwipeOutput, SwipeStreamPlan};
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

use openlogi_inject::{GestureMotion, GesturePhase};

/// One routed outcome of feeding a frame (or a stroke boundary) to a
/// session's touchpad runtime.
#[derive(Debug, Default, PartialEq)]
enum TouchpadOutput {
    /// Nothing to dispatch for this frame.
    #[default]
    Idle,
    /// A committed gesture trigger with its resolved action.
    Action { trigger: ButtonId, action: Action },
    /// One synthesized two-finger scroll frame: the centroid's travel in
    /// micrometres.
    Scroll { dx_um: i64, dy_um: i64 },
    /// The scroll stream closed, carrying the exit velocity (centroid
    /// micrometres per second, an exponential average over the stroke) that
    /// seeds momentum on a clean end; a cancellation carries none.
    ScrollEnd {
        exit_velocity_um_per_s: Option<(f64, f64)>,
    },
}

/// Time constant of the exit-velocity filter's release phase: how long a
/// slowdown before lift-off still contributes to the glide.
const VELOCITY_RELEASE_TAU_S: f64 = 0.2;

/// Everything one touchpad frame (or stroke boundary) produces: the routed
/// scroll/action output plus a DockSwipe stream step, which may accompany
/// either — a committed swipe opens its stream instead of routing an action,
/// while an unrelated scroll frame routes and leaves the stream untouched.
#[derive(Debug, Default, PartialEq)]
struct TouchpadOutcome {
    routed: TouchpadOutput,
    stream: SwipeOutput,
}

/// How long tap synthesis stays suppressed after a glide ends, however it
/// ended — a touch landing to stop the glide must not fire a tap. Options+
/// runs the same 500 ms window from its scroll-inertia stop.
const TAP_SUPPRESSION_AFTER_GLIDE: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Default)]
struct TouchpadRuntime {
    recognizer: TouchpadGestureRecognizer,
    frozen_bindings: Option<BTreeMap<ButtonId, Action>>,
    frozen_actions_enabled: bool,
    /// Whether this stroke already opened its scroll stream, so the next
    /// delta knows to continue rather than begin one.
    scroll_streaming: bool,
    /// Smoothed centroid velocity over the stroke, micrometres per second:
    /// fast-attack, slow-release with a ~200 ms time constant — a brief
    /// slowdown before lift-off must not kill a glide, but a deliberate
    /// stop-and-hold must.
    scroll_velocity_um_per_s: (f64, f64),
    /// Timestamp of the last frame seen this stroke, the dt baseline for the
    /// velocity filter — recorded on every frame, so the first streamed
    /// delta (which already spans one frame of travel) gets a real dt too.
    last_frame_us: Option<u64>,
    /// A committed swipe streaming its DockSwipe animation, if any.
    stream: Option<ActiveSwipe>,
    /// The stroke's banked spread, seeding a pinch commit with the travel
    /// the recognizer threshold consumed.
    spread_bank: SpreadBank,
    /// Until when tap resolution stays suppressed, armed when a glide ends.
    taps_suppressed_until: Option<std::time::Instant>,
}

impl TouchpadRuntime {
    fn update(
        &mut self,
        frame: &TouchFrame,
        current_bindings: &BTreeMap<ButtonId, Action>,
        actions_enabled: bool,
        dock_swipe_streaming: bool,
    ) -> TouchpadOutcome {
        if self.frozen_bindings.is_none() {
            // Stroke boundary: the first frame after end/cancel re-freezes
            // and re-banks.
            self.frozen_bindings = Some(current_bindings.clone());
            self.frozen_actions_enabled = actions_enabled;
            self.spread_bank.reset();
        }
        let mut outcome = TouchpadOutcome::default();
        let timestamp_us = frame.timestamp_us;
        #[expect(
            clippy::cast_precision_loss,
            reason = "frame timestamps stay far below 2^53 microseconds"
        )]
        let dt_us = self.last_frame_us.map_or(f64::NAN, |previous| {
            (timestamp_us.saturating_sub(previous)) as f64
        });
        self.last_frame_us = Some(timestamp_us);
        // A pressed button owns the pad however many contacts it holds: any
        // scroll stream it cut into must not glide on lift.
        if frame.button {
            self.scroll_velocity_um_per_s = (0.0, 0.0);
        }
        self.spread_bank.fold(frame);
        match self.recognizer.update(frame) {
            GestureRecognition::Gesture(trigger)
                if self.frozen_actions_enabled && actions_enabled =>
            {
                // A streaming pair holds its discrete dispatch back as the
                // Begin-failure fallback. The plan consults the trigger's
                // sibling: a commit on an unbound direction still opens the
                // stream, and a pair the system cannot honor exactly stays
                // discrete. Magnify needs none of the macOS 27 DockSwipe
                // bridge.
                if let Some(plan) = self.swipe_plan(trigger)
                    && (dock_swipe_streaming || plan.is_magnify())
                {
                    let (swipe, seeded) =
                        ActiveSwipe::new(frame, plan, trigger, self.spread_bank.take());
                    self.stream = Some(swipe);
                    outcome.stream = seeded.unwrap_or_default();
                } else if let Some((trigger, action)) = self.action(trigger) {
                    outcome.routed = TouchpadOutput::Action { trigger, action };
                }
            }
            GestureRecognition::Scroll { dx_um, dy_um } => {
                // Scrolling replaces the firmware translation the capture
                // switched off, so it flows regardless of action bindings.
                self.scroll_streaming = true;
                self.track_scroll_velocity(dt_us, dx_um, dy_um);
                outcome.routed = TouchpadOutput::Scroll { dx_um, dy_um };
            }
            GestureRecognition::Pending | GestureRecognition::Gesture(_) => {}
        }
        if let Some(swipe) = &mut self.stream
            && let Some(step) = swipe.advance(frame)
        {
            outcome.stream = step;
        }
        outcome
    }

    fn end(&mut self, actions_enabled: bool) -> TouchpadOutcome {
        let suppressed = match self.taps_suppressed_until {
            Some(until) if std::time::Instant::now() < until => true,
            _ => {
                self.taps_suppressed_until = None;
                false
            }
        };
        let action = self
            .recognizer
            .end()
            .filter(|_| !suppressed && self.frozen_actions_enabled && actions_enabled)
            .and_then(|trigger| self.action(trigger));
        let terminal = self.close_scroll_stream(true);
        let (stream, never_streamed) = self
            .stream
            .take()
            .map_or((SwipeOutput::Idle, None), ActiveSwipe::release);
        self.frozen_bindings = None;
        self.frozen_actions_enabled = false;
        TouchpadOutcome {
            routed: terminal.unwrap_or_else(|| {
                action
                    .or(never_streamed)
                    .map_or(TouchpadOutput::Idle, |(trigger, action)| {
                        TouchpadOutput::Action { trigger, action }
                    })
            }),
            stream,
        }
    }

    /// Cancel the stroke but not a running animation: dropped-frame cancels
    /// fire mid-stroke, and the real end still arrives as `TouchpadEnd`.
    fn cancel(&mut self) -> TouchpadOutcome {
        let terminal = self.close_scroll_stream(false);
        self.recognizer.cancel();
        self.frozen_bindings = None;
        self.frozen_actions_enabled = false;
        TouchpadOutcome {
            routed: terminal.unwrap_or(TouchpadOutput::Idle),
            stream: SwipeOutput::Idle,
        }
    }

    /// Terminate both the stroke and a running animation — the
    /// session-scoped teardown path.
    fn terminate(&mut self) -> TouchpadOutcome {
        let outcome = self.cancel();
        TouchpadOutcome {
            routed: outcome.routed,
            stream: self
                .stream
                .take()
                .map_or(SwipeOutput::Idle, ActiveSwipe::terminate),
        }
    }

    /// Terminate an open scroll stream, if any. Scrolling travelled past the
    /// tap limits, so a scrolled stroke can never also resolve a tap and the
    /// two outcomes never compete. A cancellation discards the velocity —
    /// momentum must not grow out of a stroke the stream rejected.
    fn close_scroll_stream(&mut self, ended: bool) -> Option<TouchpadOutput> {
        let velocity = self.scroll_velocity_um_per_s;
        self.scroll_velocity_um_per_s = (0.0, 0.0);
        self.last_frame_us = None;
        self.scroll_streaming.then(|| {
            self.scroll_streaming = false;
            TouchpadOutput::ScrollEnd {
                exit_velocity_um_per_s: ended.then_some(velocity),
            }
        })
    }

    /// Fold one frame's delta into the exit-velocity filter.
    #[expect(
        clippy::cast_precision_loss,
        reason = "per-frame deltas stay far below 2^53 micrometres"
    )]
    fn track_scroll_velocity(&mut self, dt_us: f64, dx: i64, dy: i64) {
        if !dt_us.is_finite() || dt_us <= 0.0 {
            return;
        }
        let seconds = dt_us / 1_000_000.0;
        let raw = (dx as f64 / seconds, dy as f64 / seconds);
        // Release weight normalized by the actual frame gap: a fixed
        // per-frame factor made the memory span cadence-dependent — a ~1 s
        // hold before lift still glided at 130 Hz. τ ≈ 200 ms.
        let release = (-seconds / VELOCITY_RELEASE_TAU_S).exp();
        let attack = 1.0 - release;
        let smoothed = &mut self.scroll_velocity_um_per_s;
        for (smoothed, raw) in [(&mut smoothed.0, raw.0), (&mut smoothed.1, raw.1)] {
            // A same-axis reversal is new intent, not noise — adopt it at
            // once instead of averaging against the stale direction.
            if *smoothed * raw < 0.0 || raw.abs() > smoothed.abs() {
                *smoothed = raw;
            } else {
                *smoothed = *smoothed * release + raw * attack;
            }
        }
    }

    /// Close the failed stream and recover the action whose animation failed
    /// to begin, for the discrete fallback.
    fn begin_failed(&mut self, opening_progress: f64) -> Option<(ButtonId, Action)> {
        self.stream.take()?.opening_binding(opening_progress)
    }

    /// The pair plan a committed trigger streams under, if the stroke's
    /// frozen bindings let the system honor the pair.
    fn swipe_plan(&self, trigger: ButtonId) -> Option<SwipeStreamPlan> {
        SwipeStreamPlan::for_trigger(trigger, self.frozen_bindings.as_ref()?)
    }

    fn action(&self, trigger: ButtonId) -> Option<(ButtonId, Action)> {
        self.frozen_bindings
            .as_ref()?
            .get(&trigger)
            .cloned()
            .map(|action| (trigger, action))
    }

    /// Suppress tap resolution for `window`, from a glide that just ended.
    fn suppress_taps(&mut self, window: std::time::Duration) {
        self.taps_suppressed_until = Some(std::time::Instant::now() + window);
    }
}

#[derive(Default)]
struct SessionTouchpads(HashMap<HidppSessionId, TouchpadRuntime>);

impl SessionTouchpads {
    fn for_session(&mut self, session: &HidppSessionId) -> &mut TouchpadRuntime {
        self.0.entry(session.clone()).or_default()
    }

    fn begin_failed(
        &mut self,
        session: &HidppSessionId,
        opening_progress: f64,
    ) -> Option<(ButtonId, Action)> {
        self.0
            .get_mut(session)
            .and_then(|runtime| runtime.begin_failed(opening_progress))
    }

    fn cancel_session(&mut self, session: &HidppSessionId) -> TouchpadOutcome {
        self.0
            .remove(session)
            .map_or_else(TouchpadOutcome::default, |mut runtime| runtime.terminate())
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
        // The terminated swipe animation springs back (its stream step is
        // the outcome's only live half); the routed side is a dead terminal
        // with no momentum and nothing to post.
        let outcome = self.touchpads.cancel_session(session);
        Self::execute_touchpad_stream(session.epoch(), &outcome.stream, session.device_key());
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
                self.dispatch_scroll(session, plan, increments, resolution, key);
            }
            CapturedInput::ThumbwheelDirection { .. } => {
                unreachable!("thumb-wheel direction reports return before dispatch")
            }
            CapturedInput::TouchpadFrame(frame) => {
                // Touch re-lands, or the glide decayed out since the last
                // frame: either way the tail is over, and the stopping
                // touch must not resolve into a tap.
                if let Some(momentum) = self.momentum.take() {
                    momentum.stop();
                    self.touchpads
                        .for_session(session)
                        .suppress_taps(TAP_SUPPRESSION_AFTER_GLIDE);
                }
                let tuning = TouchpadScrollTuning::from_plan(plan);
                let dock_swipe_streaming = openlogi_inject::dock_swipe_supported();
                let outcome = self.touchpads.for_session(session).update(
                    &frame,
                    &plan.touchpad_bindings,
                    touchpad_actions_enabled,
                    dock_swipe_streaming,
                );
                Self::execute_touchpad_outcome(
                    &self.outputs,
                    &mut self.touchpads,
                    tuning,
                    session,
                    outcome,
                    key,
                );
            }
            CapturedInput::TouchpadEnd => {
                self.end_touchpad_stroke(session, plan, key, touchpad_actions_enabled);
            }
            CapturedInput::TouchpadCancel => {
                self.cancel_touchpad_stroke(session, plan, key);
            }
            CapturedInput::TouchpadDroppedFrames(_) => {}
        }
    }

    /// Route one thumb-wheel rotation through the per-session accumulator:
    /// post the scaled scroll, or fire the bound action once its travel
    /// crosses the threshold.
    fn dispatch_scroll(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        increments: i16,
        resolution: WheelResolution,
        key: &str,
    ) {
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

    /// Cancel one touchpad stroke: stop any gliding tail, spring back an
    /// open swipe animation, and route the cancelled scroll terminal.
    fn cancel_touchpad_stroke(&mut self, session: &HidppSessionId, plan: &DispatchPlan, key: &str) {
        self.stop_momentum();
        let tuning = TouchpadScrollTuning::from_plan(plan);
        let outcome = self.touchpads.for_session(session).cancel();
        Self::execute_touchpad_stream(session.epoch(), &outcome.stream, key);
        Self::route_touchpad_output(&self.outputs, tuning, key, outcome.routed);
    }

    /// End one touchpad stroke: execute the swipe stream's release step,
    /// route the terminal (a tap that survived the scroll travel limits),
    /// and, when the lift-off was fast enough, hand the exit velocity to a
    /// momentum tail.
    fn end_touchpad_stroke(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        key: &str,
        actions_enabled: bool,
    ) {
        let tuning = TouchpadScrollTuning::from_plan(plan);
        let outcome = self.touchpads.for_session(session).end(actions_enabled);
        let exit_velocity = match &outcome.routed {
            TouchpadOutput::ScrollEnd {
                exit_velocity_um_per_s,
                ..
            } => *exit_velocity_um_per_s,
            _ => None,
        };
        Self::execute_touchpad_stream(session.epoch(), &outcome.stream, key);
        Self::route_touchpad_output(&self.outputs, tuning, key, outcome.routed);
        if let Some(exit_velocity) = exit_velocity {
            // Replacing the handle does not stop the old tail (dropping it
            // never does) — without this, two glides stack their deltas.
            self.stop_momentum();
            self.momentum = TouchpadMomentum::start(tuning, exit_velocity);
        }
    }

    fn execute_touchpad_outcome(
        outputs: &GestureOutputs,
        touchpads: &mut SessionTouchpads,
        tuning: TouchpadScrollTuning,
        session: &HidppSessionId,
        outcome: TouchpadOutcome,
        key: &str,
    ) {
        let owner = session.epoch();
        match &outcome.stream {
            SwipeOutput::Begin {
                motion: GestureMotion::Zoom,
                progress,
            } => {
                // Began carries no scale — apps accumulate Changed deltas —
                // so the opening delta follows as the first Changed frame.
                if openlogi_inject::post_magnify(GesturePhase::Began, 0.0)
                    && openlogi_inject::post_magnify(GesturePhase::Changed, *progress)
                {
                    debug!(key, "touchpad pinch → native magnify zoom");
                } else {
                    tracing::warn!(key, "native magnify zoom failed to begin");
                    if let Some((trigger, action)) = touchpads.begin_failed(session, *progress) {
                        debug!(key, %trigger, action = %action.label(), "touchpad pinch → discrete fallback");
                        outputs.actions.dispatch(&action, Some(key));
                    }
                }
            }
            SwipeOutput::Begin { motion, progress } => {
                if openlogi_inject::post_dock_swipe(owner, *motion, GesturePhase::Began, *progress)
                {
                    debug!(key, ?motion, "touchpad swipe → native DockSwipe animation");
                } else {
                    tracing::warn!(key, ?motion, "native dock swipe failed to begin");
                    if let Some((trigger, action)) = touchpads.begin_failed(session, *progress) {
                        debug!(key, %trigger, action = %action.label(), "touchpad swipe → discrete fallback");
                        outputs.actions.dispatch(&action, Some(key));
                    }
                }
            }
            stream => Self::execute_touchpad_stream(owner, stream, key),
        }
        Self::route_touchpad_output(outputs, tuning, key, outcome.routed);
    }

    /// Use the capture-session epoch as the global DockSwipe owner.
    fn execute_touchpad_stream(owner: u64, stream: &SwipeOutput, key: &str) {
        match *stream {
            SwipeOutput::Idle => {}
            SwipeOutput::Begin { .. } => unreachable!("handled by execute_touchpad_outcome"),
            SwipeOutput::Advance {
                motion: GestureMotion::Zoom,
                delta,
            } => {
                openlogi_inject::post_magnify(GesturePhase::Changed, delta);
            }
            SwipeOutput::Advance { motion, delta } => {
                openlogi_inject::post_dock_swipe(owner, motion, GesturePhase::Changed, delta);
            }
            SwipeOutput::Finish {
                motion: GestureMotion::Zoom,
                end,
            } => {
                openlogi_inject::post_magnify(end.as_gesture_phase(), 0.0);
            }
            SwipeOutput::Finish {
                motion,
                end: SwipeEnd::AtRelease,
            } => {
                openlogi_inject::post_dock_swipe(owner, motion, GesturePhase::End, 0.0);
            }
            SwipeOutput::Finish {
                motion,
                end: SwipeEnd::Cancelled,
            } => {
                debug!(key, ?motion, "touchpad swipe animation cancelled");
                openlogi_inject::post_dock_swipe(owner, motion, GesturePhase::Cancel, 0.0);
            }
        }
    }

    fn route_touchpad_output(
        outputs: &GestureOutputs,
        tuning: TouchpadScrollTuning,
        key: &str,
        outcome: TouchpadOutput,
    ) {
        match outcome {
            TouchpadOutput::Action { trigger, action } => {
                debug!(key, %trigger, action = %action.label(), "touchpad gesture → action");
                outputs.actions.dispatch(&action, Some(key));
            }
            TouchpadOutput::Scroll { dx_um, dy_um } => {
                super::post_touchpad_scroll(tuning, dx_um, dy_um);
            }
            // The terminal's exit velocity was consumed above; idle posts
            // nothing.
            TouchpadOutput::Idle | TouchpadOutput::ScrollEnd { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests;
