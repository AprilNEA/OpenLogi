//! Resolve captured HID++ inputs against the active per-device plan.

mod hold;
mod wheel;

use std::collections::HashMap;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::CapturedInput;
use tracing::{debug, warn};

use self::hold::HoldSessions;
use self::wheel::{ScrollScale, WheelAccumulators, WheelOutput, WheelRotation};
use super::GestureOutputs;
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

/// Input routing plus the per-session state retained between
/// captured events. Capture-session lifecycle remains owned by the parent.
pub(super) struct InputDispatcher {
    hook_maps: SharedHookMaps,
    outputs: GestureOutputs,
    wheels: SessionWheels,
    gesture_presses: GesturePresses,
    holds: HoldSessions,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            gesture_presses: GesturePresses::default(),
            holds: HoldSessions::default(),
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
    ///
    /// A still-live epoch (dispatch-plan refresh) ends the hold but stays
    /// writable so a later press can begin again. Call [`Self::retire_session`]
    /// when the epoch is dead.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) {
        if let Some(command) = self.holds.end_open(session) {
            hold::emit(command);
        }
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.gesture_presses.cancel_session(session);
    }

    /// End a live hold and lock the epoch. Retirement still owns drain
    /// inputs, so a late HoldBegin must not open a pinch nothing closes.
    pub(super) fn lock_holds(&mut self, session: &HidppSessionId) {
        if let Some(command) = self.holds.close_session(session) {
            hold::emit(command);
        }
    }

    /// Epoch is gone. Late HoldBegin/HoldMotion must not reopen inject.
    pub(super) fn retire_session(&mut self, session: &HidppSessionId) {
        if let Some(command) = self.holds.close_session(session) {
            hold::emit(command);
        }
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.gesture_presses.cancel_session(session);
    }

    /// Watcher or process teardown: end every hold and flush inject.
    pub(super) fn shutdown_holds(&mut self) {
        for command in self.holds.close_all() {
            hold::emit(command);
        }
        hold::flush_inject();
    }

    /// Route one captured input from `session` to its bound action or
    /// re-synthesised scroll output.
    pub(super) fn dispatch(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        input: CapturedInput,
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
                self.dispatch_button_down(session, plan, button);
            }
            CapturedInput::ButtonUp(button) => {
                self.outputs.actions.try_hidpp_button_up(session, button);
                self.gesture_presses.end(session, button);
            }
            CapturedInput::ButtonPulse(button) => {
                self.dispatch_button_pulse(session, plan, button);
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
            CapturedInput::HoldBegin(_)
            | CapturedInput::HoldMotion { .. }
            | CapturedInput::HoldEnd { .. } => self.dispatch_hold(session, plan, input),
        }
    }

    fn dispatch_button_down(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
    ) {
        let key = session.device_key();
        if warn_suppressed_hold_click(key, plan, button) {
            return;
        }
        // A raw-XY gesture source owns its click/swipe map; its physical
        // lifecycle is still tracked, but it must not also fire the
        // single-action projection on down.
        let is_gesture = plan.gesture_bindings.contains_key(&button)
            || plan.side_gesture_bindings.contains_key(&button);
        let binding = hidpp_click_binding(plan, button);
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

    fn dispatch_button_pulse(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
    ) {
        let key = session.device_key();
        if warn_suppressed_hold_click(key, plan, button) {
            return;
        }
        let binding = hidpp_click_binding(plan, button);
        if let Some(binding) = binding {
            debug!(key, ?button, action = %binding.click_action().label(), "HID++ button pulse → binding");
        } else {
            debug!(key, ?button, "HID++ button pulse with no binding — ignored");
        }
        self.outputs
            .actions
            .dispatch_hidpp_button_pulse(session, button, binding);
    }

    fn dispatch_hold(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        input: CapturedInput,
    ) {
        let command = match input {
            CapturedInput::HoldBegin(button) => self.holds.begin(session, button, plan),
            CapturedInput::HoldMotion { button, dx, dy } => {
                self.holds.motion(session, button, dx, dy)
            }
            CapturedInput::HoldEnd { button, release } => self.holds.end(session, button, release),
            _ => unreachable!("dispatch_hold is only called for Hold*"),
        };
        if let Some(command) = command {
            hold::emit(command);
        }
    }
}

/// A hold-mode binding on the click path is a failed delivery, not a
/// one-shot scroll. When hold is unarmed the plan plain-diverts the CID
/// so firmware cannot keep scrolling; this rejects the resulting press.
fn hidpp_hold_click_suppressed(plan: &DispatchPlan, button: ButtonId) -> Option<&Action> {
    plan.hold_bindings.get(&button)
}

/// Log and refuse a hold-mode binding that arrived on the click path.
fn warn_suppressed_hold_click(key: &str, plan: &DispatchPlan, button: ButtonId) -> bool {
    let Some(action) = hidpp_hold_click_suppressed(plan, button) else {
        return false;
    };
    warn!(
        key,
        ?button,
        action = %action.label(),
        "hold-mode action cannot be delivered as a click — ignored"
    );
    true
}

/// Click binding that would be handed to the action runtime. Hold-mode
/// buttons are never a click, even when they still appear in `bindings`.
fn hidpp_click_binding(plan: &DispatchPlan, button: ButtonId) -> Option<&Binding> {
    if hidpp_hold_click_suppressed(plan, button).is_some() {
        return None;
    }
    let is_gesture = plan.gesture_bindings.contains_key(&button)
        || plan.side_gesture_bindings.contains_key(&button);
    if is_gesture {
        return None;
    }
    plan.bindings.get(&button)
}

#[cfg(test)]
mod tests;
