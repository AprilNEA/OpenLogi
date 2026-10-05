//! Resolve captured HID++ inputs against the active per-device plan.

mod space_swipe;
mod wheel;

use std::collections::HashMap;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::CapturedInput;
use openlogi_hid::thumbwheel::WheelResolution;
use tracing::debug;

use openlogi_inject::SpaceSwipePhase;

use self::space_swipe::{Frame, SpaceProbe, SpaceSwipes};
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

/// Posts one live Space-transition frame to the Dock. Unit tests never reach
/// the Dock: their frames report unposted, so desktop swipes take the one-shot
/// path they always have.
#[cfg(not(test))]
const POST_SPACE_SWIPE: fn(f64, SpaceSwipePhase) -> bool = openlogi_inject::post_space_swipe;
#[cfg(test)]
const POST_SPACE_SWIPE: fn(f64, SpaceSwipePhase) -> bool = |_, _| false;
/// What live transitions read about the desktop; tests read nothing.
#[cfg(not(test))]
const SPACE_PROBE: SpaceProbe = SpaceProbe::NATIVE;
#[cfg(test)]
const SPACE_PROBE: SpaceProbe = SpaceProbe {
    neighbors: || None,
    changes: || None,
};

/// The swipe that reverses `direction`, for the opposite desktop binding.
fn reverse(direction: GestureDirection) -> GestureDirection {
    match direction {
        GestureDirection::Left => GestureDirection::Right,
        GestureDirection::Right => GestureDirection::Left,
        GestureDirection::Up => GestureDirection::Down,
        GestureDirection::Down => GestureDirection::Up,
        GestureDirection::Click => GestureDirection::Click,
    }
}

/// Input routing plus the per-session state retained between
/// captured events. Capture-session lifecycle remains owned by the parent.
pub(super) struct InputDispatcher {
    hook_maps: SharedHookMaps,
    outputs: GestureOutputs,
    wheels: SessionWheels,
    gesture_presses: GesturePresses,
    spaces: SpaceSwipes,
    post_space: fn(f64, SpaceSwipePhase) -> bool,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            gesture_presses: GesturePresses::default(),
            spaces: SpaceSwipes::new(space_swipe::configured_travel(), SPACE_PROBE),
            post_space: POST_SPACE_SWIPE,
        }
    }

    /// Route live Space frames through `post` instead of the Dock.
    #[cfg(test)]
    pub(super) fn with_space_output(
        mut self,
        travel: f64,
        post: fn(f64, SpaceSwipePhase) -> bool,
    ) -> Self {
        self.spaces = SpaceSwipes::new(Some(travel), SPACE_PROBE);
        self.post_space = post;
        self
    }

    /// Post live Space frames in order, stopping at the first the Dock path
    /// refuses.
    fn post_space_frames(&self, key: &str, button: ButtonId, frames: &[Frame]) {
        for frame in frames {
            if frame.phase != SpaceSwipePhase::Changed {
                debug!(key, %button, phase = ?frame.phase, progress = frame.progress, "live Space frame");
            }
            if !(self.post_space)(frame.progress, frame.phase) {
                debug!(key, %button, phase = ?frame.phase, "live Space frame not posted");
                return;
            }
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

    /// Whether `session` still holds an admitted gesture press for `button`.
    #[cfg(test)]
    pub(super) fn holds_gesture_press(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.gesture_presses.get(session, button).is_some()
    }

    /// Cancel every input lifecycle retained for one capture session.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) {
        let frames = self.spaces.cancel_session(session);
        self.post_space_frames(session.device_key(), ButtonId::GestureButton, &frames);
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.gesture_presses.cancel_session(session);
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
                if self.gesture_presses.get(session, button).is_none() {
                    debug!(key, %button, ?direction, "gesture from a canceled button lifecycle — ignored");
                    return;
                }
                // A hold driving a live transition moves it with motion; its
                // later swipe commits are not separate actions.
                if self.spaces.is_active(session, button) {
                    return;
                }
                let map = plan
                    .gesture_bindings
                    .get(&button)
                    .or_else(|| plan.side_gesture_bindings.get(&button));
                let Some(action) = map.and_then(|map| map.get(&direction)) else {
                    debug!(key, %button, ?direction, "gesture with no binding — ignored");
                    return;
                };
                debug!(key, %button, ?direction, action = %action.label(), "gesture → action");
                let opposite = map.and_then(|map| map.get(&reverse(direction)));
                if let Some(frame) =
                    self.spaces
                        .begin(session, button, direction, action, opposite, Instant::now())
                {
                    if (self.post_space)(frame.progress, frame.phase) {
                        debug!(key, %button, ?direction, "live Space transition began");
                        return;
                    }
                    // No live transition here: the one-shot switch instead.
                    self.spaces.abandon(session, button);
                }
                let Some(press) = self.gesture_presses.get(session, button) else {
                    return;
                };
                if !self
                    .outputs
                    .actions
                    .try_dispatch_while_pressed(press, action)
                {
                    debug!(key, %button, ?direction, "gesture press no longer active — ignored");
                }
            }
            CapturedInput::GestureMotion { button, dx, .. } => {
                let frames = self.spaces.motion(session, button, dx, Instant::now());
                self.post_space_frames(key, button, &frames);
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
                let frames = self.spaces.end(session, button, Instant::now());
                self.post_space_frames(key, button, &frames);
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
            } => self.dispatch_wheel(session, plan, increments, resolution),
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
    ) {
        let Some(rotation) = WheelRotation::from_increments(increments) else {
            return;
        };
        let key = session.device_key();
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
                self.outputs.actions.dispatch_pointer_action(
                    action,
                    Some(key),
                    plan.pointer_target,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
