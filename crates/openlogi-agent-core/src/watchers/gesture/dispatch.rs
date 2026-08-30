//! Resolve captured HID++ inputs against the active per-device plan.

mod wheel;

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::touchpad::{GestureRecognition, TouchFrame, TouchpadGestureRecognizer};
use openlogi_hid::CapturedInput;
use tracing::debug;

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

#[derive(Default)]
struct TouchpadRuntime {
    recognizer: TouchpadGestureRecognizer,
    frozen_bindings: Option<BTreeMap<ButtonId, Action>>,
    frozen_actions_enabled: bool,
}

impl TouchpadRuntime {
    fn update(
        &mut self,
        frame: &TouchFrame,
        current_bindings: &BTreeMap<ButtonId, Action>,
        actions_enabled: bool,
    ) -> Option<(ButtonId, Action)> {
        if self.frozen_bindings.is_none() {
            self.frozen_bindings = Some(current_bindings.clone());
            self.frozen_actions_enabled = actions_enabled;
        }
        match self.recognizer.update(frame) {
            GestureRecognition::Gesture(trigger)
                if self.frozen_actions_enabled && actions_enabled =>
            {
                self.action(trigger)
            }
            GestureRecognition::Pending
            | GestureRecognition::NativeScroll
            | GestureRecognition::Gesture(_) => None,
        }
    }

    fn end(&mut self, actions_enabled: bool) -> Option<(ButtonId, Action)> {
        let action = self
            .recognizer
            .end()
            .filter(|_| self.frozen_actions_enabled && actions_enabled)
            .and_then(|trigger| self.action(trigger));
        self.frozen_bindings = None;
        self.frozen_actions_enabled = false;
        action
    }

    fn cancel(&mut self) {
        self.recognizer.cancel();
        self.frozen_bindings = None;
        self.frozen_actions_enabled = false;
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
    touchpads: SessionTouchpads,
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
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.gesture_presses.cancel_session(session);
        self.touchpads.cancel_session(session);
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
                Self::dispatch_touchpad_frame(
                    &mut self.touchpads,
                    &self.outputs,
                    session,
                    key,
                    &frame,
                    &plan.touchpad_bindings,
                    touchpad_actions_enabled,
                );
            }
            CapturedInput::TouchpadEnd => {
                Self::end_touchpad_stroke(
                    &mut self.touchpads,
                    &self.outputs,
                    session,
                    key,
                    touchpad_actions_enabled,
                );
            }
            CapturedInput::TouchpadCancel => {
                self.touchpads.for_session(session).cancel();
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

    fn dispatch_touchpad_frame(
        touchpads: &mut SessionTouchpads,
        outputs: &GestureOutputs,
        session: &HidppSessionId,
        key: &str,
        frame: &TouchFrame,
        bindings: &BTreeMap<ButtonId, Action>,
        actions_enabled: bool,
    ) {
        let action = touchpads
            .for_session(session)
            .update(frame, bindings, actions_enabled);
        Self::dispatch_touchpad_action(outputs, key, action);
    }

    fn end_touchpad_stroke(
        touchpads: &mut SessionTouchpads,
        outputs: &GestureOutputs,
        session: &HidppSessionId,
        key: &str,
        actions_enabled: bool,
    ) {
        let action = touchpads.for_session(session).end(actions_enabled);
        Self::dispatch_touchpad_action(outputs, key, action);
    }

    fn dispatch_touchpad_action(
        outputs: &GestureOutputs,
        key: &str,
        action: Option<(ButtonId, Action)>,
    ) {
        let Some((trigger, action)) = action else {
            return;
        };
        debug!(key, %trigger, action = %action.label(), "touchpad gesture → action");
        outputs.actions.dispatch(&action, Some(key));
    }
}

#[cfg(test)]
mod tests;
