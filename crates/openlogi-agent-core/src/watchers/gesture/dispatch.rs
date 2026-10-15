//! Resolve captured HID++ inputs against the active per-device plan.

mod wheel;
#[cfg(any(target_os = "macos", test))]
mod zoom;

use std::collections::HashMap;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::CapturedInput;
use openlogi_hid::thumbwheel::WheelResolution;
use tracing::debug;

use self::wheel::{ScrollScale, WheelAccumulators, WheelOutput, WheelRotation};
use super::GestureOutputs;
use crate::capture_plan::DispatchPlan;
use crate::runtime::hook::SharedHookMaps;
use crate::runtime::{HidppSessionId, PressToken};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WheelAdmission {
    Active,
    Draining,
}

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

    #[cfg(not(target_os = "macos"))]
    fn cancel_zoom_session(&mut self, session: &HidppSessionId) {
        if let Some(wheel) = self.0.get_mut(session) {
            wheel.clear_zoom();
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn clear_zoom(&mut self) {
        for wheel in self.0.values_mut() {
            wheel.clear_zoom();
        }
    }
}

/// Input routing plus the per-session state retained between
/// captured events. Capture-session lifecycle remains owned by the parent.
pub(super) struct InputDispatcher {
    hook_maps: SharedHookMaps,
    outputs: GestureOutputs,
    wheels: SessionWheels,
    gesture_presses: GesturePresses,
    #[cfg(target_os = "macos")]
    zooms: Option<zoom::worker::Worker>,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        #[cfg(target_os = "macos")]
        let zooms = zoom::worker::Worker::spawn(outputs.clone())
            .map_err(|error| tracing::warn!(%error, "native Zoom worker unavailable"))
            .ok();
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            gesture_presses: GesturePresses::default(),
            #[cfg(target_os = "macos")]
            zooms,
        }
    }

    /// Publish a hardware polarity observation into the OS-hook snapshot.
    fn record_thumbwheel_direction(&self, key: &str, input: CapturedInput) -> bool {
        let CapturedInput::ThumbwheelDirection {
            positive_is_forward,
        } = input
        else {
            return false;
        };
        if let Ok(mut maps) = self.hook_maps.write() {
            maps.thumbwheel_positive_is_forward
                .insert(key.to_owned(), positive_is_forward);
        }
        true
    }

    /// Cancel every input lifecycle retained for one capture session.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) {
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        #[cfg(target_os = "macos")]
        if let Some(zooms) = &mut self.zooms {
            zooms.cancel(session);
        }
        self.gesture_presses.cancel_session(session);
    }

    pub(super) fn pause_zoom(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(zooms) = &mut self.zooms {
            zooms.pause();
        }
        #[cfg(not(target_os = "macos"))]
        self.wheels.clear_zoom();
    }

    #[cfg_attr(
        not(target_os = "macos"),
        expect(clippy::unused_self, reason = "native zoom state exists only on macOS")
    )]
    pub(super) fn resume_zoom(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(zooms) = &mut self.zooms {
            zooms.resume();
        }
    }
    pub(super) fn cancel_zoom_session(&mut self, session: &HidppSessionId) {
        #[cfg(target_os = "macos")]
        if let Some(zooms) = &mut self.zooms {
            zooms.cancel(session);
        }
        #[cfg(not(target_os = "macos"))]
        self.wheels.cancel_zoom_session(session);
    }

    pub(super) fn shutdown_zoom(&mut self) -> impl std::future::Future<Output = ()> + use<> {
        #[cfg(target_os = "macos")]
        let cleanup = self
            .zooms
            .as_mut()
            .map(zoom::worker::Worker::shutdown_and_wait);
        #[cfg(not(target_os = "macos"))]
        self.wheels.clear_zoom();
        async move {
            #[cfg(target_os = "macos")]
            if let Some(cleanup) = cleanup {
                cleanup.await;
            }
        }
    }

    /// Route one captured input from `session` to its bound action or
    /// re-synthesised scroll output.
    pub(super) fn dispatch(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        input: CapturedInput,
        admission: WheelAdmission,
    ) {
        let key = session.device_key();
        if self.record_thumbwheel_direction(key, input) {
            return;
        }
        match input {
            CapturedInput::Gesture(button, direction) => {
                let Some(press) = self.gesture_presses.get(session, button) else {
                    debug!(key, %button, ?direction, "gesture from a canceled button lifecycle — ignored");
                    return;
                };
                if let Some(action) = plan
                    .gesture_bindings
                    .get(&button)
                    .or_else(|| plan.side_gesture_bindings.get(&button))
                    .and_then(|map| map.get(&direction))
                {
                    debug!(key, %button, ?direction, action = %action.label(), "gesture → action");
                    if !self
                        .outputs
                        .actions
                        .try_dispatch_while_pressed(press, action)
                    {
                        debug!(key, %button, ?direction, "gesture press no longer active — ignored");
                    }
                } else {
                    debug!(key, %button, ?direction, "gesture with no binding — ignored");
                }
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
                let press = self.outputs.actions.try_hidpp_button_down(
                    session,
                    button,
                    binding,
                    plan.pointer_target,
                );
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
                let binding = plan.bindings.get(&button);
                if let Some(binding) = binding {
                    debug!(key, ?button, action = %binding.click_action().label(), "HID++ button pulse → binding");
                } else {
                    debug!(key, ?button, "HID++ button pulse with no binding — ignored");
                }
                self.outputs.actions.dispatch_hidpp_button_pulse(
                    session,
                    button,
                    binding,
                    plan.pointer_target,
                );
            }
            CapturedInput::Scroll {
                increments,
                resolution,
                status,
            } => self.dispatch_wheel(session, plan, increments, resolution, status, admission),
            CapturedInput::ThumbwheelDirection { .. } => {
                unreachable!("thumb-wheel direction reports return before dispatch")
            }
        }
    }

    fn dispatch_wheel(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        increments: i16,
        resolution: WheelResolution,
        status: openlogi_hid::thumbwheel::RotationStatus,
        admission: WheelAdmission,
    ) {
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        if increments == 0 && status == openlogi_hid::thumbwheel::RotationStatus::Stop {
            if admission == WheelAdmission::Draining {
                self.cancel_zoom_session(session);
            } else if let Some(zooms) = &mut self.zooms {
                zooms.stop(session, now);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            if admission == WheelAdmission::Draining {
                self.wheels.cancel_zoom_session(session);
            } else if increments == 0 {
                self.wheels
                    .for_session(session)
                    .prepare_zoom_report(status, 0, now);
            }
        }
        let Some(rotation) = WheelRotation::from_increments(increments) else {
            return;
        };
        let key = session.device_key();
        let button = rotation.button();
        let configuration = WheelConfiguration::for_plan(plan);
        let action = configuration.action(rotation);
        #[cfg(not(target_os = "macos"))]
        if matches!(action, Action::ZoomIn | Action::ZoomOut)
            && (admission == WheelAdmission::Draining
                || !self
                    .wheels
                    .for_session(session)
                    .prepare_zoom_report(status, increments, now))
        {
            return;
        }
        #[cfg(target_os = "macos")]
        if matches!(action, Action::ZoomIn | Action::ZoomOut) {
            if admission == WheelAdmission::Draining {
                self.cancel_zoom_session(session);
            } else if let Some(zooms) = &mut self.zooms {
                zooms.motion(
                    session,
                    zoom::worker::Input {
                        configuration: configuration.clone(),
                        foreground_app: self.outputs.foreground_app_id(),
                        pointer: plan.pointer_target,
                        rotation,
                        magnitude: f64::from(i32::from(increments).abs()),
                        now,
                        stopped: status == openlogi_hid::thumbwheel::RotationStatus::Stop,
                    },
                );
            }
            return;
        }
        #[cfg(target_os = "macos")]
        self.cancel_zoom_session(session);
        let wheels = self.wheels.for_session(session);
        match wheels.advance(
            rotation,
            action,
            ScrollScale::new(resolution, configuration.sensitivity),
            now,
        ) {
            WheelOutput::Idle => {}
            WheelOutput::Scroll(delta) => self.outputs.post_scroll(session, delta),
            WheelOutput::FireAction(repeats) => {
                debug!(key, ?button, action = %action.label(), repeats, "thumb wheel → action");
                for _ in 0..repeats {
                    self.outputs.actions.dispatch_pointer_action(
                        action,
                        Some(key),
                        plan.pointer_target,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
