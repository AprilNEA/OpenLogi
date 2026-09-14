//! Resolve captured HID++ inputs against the active per-device plan.

mod wheel;

use std::collections::HashMap;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::CapturedInput;
use tracing::debug;

use self::wheel::{ScrollScale, WheelAccumulators, WheelOutput, WheelRotation};
use super::GestureOutputs;
use crate::capture_plan::DispatchPlan;
use crate::runtime::hook::SharedHookMaps;
use crate::runtime::{HidppSessionId, PressToken};

/// The sign the Dock expects for the next Space. Raw HID++ deltas are flipped
/// relative to this when a user deliberately maps the opposite swipe direction
/// to the same desktop action, preserving the binding's semantic target.
#[cfg(any(target_os = "macos", test))]
const NEXT_DESKTOP_SPACE_SWIPE_SIGN: f64 = 1.0;
#[cfg(any(target_os = "macos", test))]
const PREVIOUS_DESKTOP_SPACE_SWIPE_SIGN: f64 = -1.0;

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

/// Every HID++ gesture source with raw-XY motion is eligible for a live Space
/// transition. The side-button map exists specifically for macOS Back/Forward
/// controls whose motion is captured through the same device-owned path.
fn is_interactive_space_source(plan: &DispatchPlan, button: ButtonId) -> bool {
    plan.gesture_bindings.contains_key(&button) || plan.side_gesture_bindings.contains_key(&button)
}

/// One native frame emitted by an active interactive Space transition.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpaceSwipeFrame {
    /// Cumulative horizontal travel since this physical gesture began.
    progress_x: f64,
    phase: openlogi_inject::InteractiveSpacePhase,
}

/// The session-local state of raw-XY gestures that macOS renders as an
/// interactive Space transition. It deliberately lives beside the capture
/// dispatcher rather than the generic action runtime: only diverted HID++
/// gesture controls preserve the motion stream needed to drive it.
#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct InteractiveSpaceTransitions {
    active: HashMap<(HidppSessionId, ButtonId), ActiveSpaceTransition>,
}

#[cfg(any(target_os = "macos", test))]
struct ActiveSpaceTransition {
    polarity: f64,
    #[cfg(target_os = "macos")]
    fallback: Action,
    began: bool,
    progress_x: f64,
}

#[cfg(any(target_os = "macos", test))]
impl InteractiveSpaceTransitions {
    fn begin(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        direction: GestureDirection,
        action: &Action,
    ) -> bool {
        let source_sign = match direction {
            GestureDirection::Left => -1.0,
            GestureDirection::Right => 1.0,
            GestureDirection::Up | GestureDirection::Down | GestureDirection::Click => {
                return false;
            }
        };
        let target_sign = match action {
            Action::NextDesktop => NEXT_DESKTOP_SPACE_SWIPE_SIGN,
            Action::PreviousDesktop => PREVIOUS_DESKTOP_SPACE_SWIPE_SIGN,
            _ => return false,
        };
        let polarity = target_sign / source_sign;
        self.active.insert(
            (session.clone(), button),
            ActiveSpaceTransition {
                polarity,
                #[cfg(target_os = "macos")]
                fallback: action.clone(),
                began: false,
                progress_x: 0.0,
            },
        );
        true
    }

    fn motion(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        delta_x: i32,
    ) -> Option<SpaceSwipeFrame> {
        let transition = self.active.get_mut(&(session.clone(), button))?;
        transition.progress_x += f64::from(delta_x) * transition.polarity;
        Some(SpaceSwipeFrame {
            progress_x: transition.progress_x,
            phase: if std::mem::replace(&mut transition.began, true) {
                openlogi_inject::InteractiveSpacePhase::Changed
            } else {
                openlogi_inject::InteractiveSpacePhase::Began
            },
        })
    }

    #[cfg(target_os = "macos")]
    fn fallback(&self, session: &HidppSessionId, button: ButtonId) -> Option<&Action> {
        self.active
            .get(&(session.clone(), button))
            .map(|transition| &transition.fallback)
    }

    fn end(&mut self, session: &HidppSessionId, button: ButtonId) -> Option<SpaceSwipeFrame> {
        self.active
            .remove(&(session.clone(), button))
            .map(|transition| SpaceSwipeFrame {
                progress_x: transition.progress_x,
                phase: openlogi_inject::InteractiveSpacePhase::Ended,
            })
    }

    fn cancel_session(&mut self, session: &HidppSessionId) -> Vec<SpaceSwipeFrame> {
        let keys: Vec<_> = self
            .active
            .keys()
            .filter(|(candidate, _)| candidate == session)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| {
                self.active.remove(&key).map(|transition| SpaceSwipeFrame {
                    progress_x: transition.progress_x,
                    phase: openlogi_inject::InteractiveSpacePhase::Cancelled,
                })
            })
            .collect()
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
    space_transitions: InteractiveSpaceTransitions,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            gesture_presses: GesturePresses::default(),
            #[cfg(target_os = "macos")]
            space_transitions: InteractiveSpaceTransitions::default(),
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
        #[cfg(target_os = "macos")]
        self.cancel_space_transitions(session);
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
                self.dispatch_gesture(session, plan, button, direction);
            }
            CapturedInput::GestureMotion {
                button,
                delta_x,
                delta_y: _,
            } => {
                #[cfg(target_os = "macos")]
                self.advance_interactive_space_transition(session, button, delta_x);
                #[cfg(not(target_os = "macos"))]
                let _ = (session, button, delta_x);
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
                #[cfg(target_os = "macos")]
                self.end_interactive_space_transition(session, button);
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
                self.outputs
                    .actions
                    .dispatch_hidpp_button_pulse(session, button, binding);
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
        }
    }

    fn dispatch_gesture(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
        direction: GestureDirection,
    ) {
        let key = session.device_key();
        let Some(press) = self.gesture_presses.get(session, button).cloned() else {
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
        // A live transition needs raw XY frames after direction resolution.
        // Both dedicated gesture maps and macOS side-button maps provide
        // those HID++ frames; OS-hook-only gesture controls keep one-shot
        // dispatch because they have no comparable motion stream.
        let is_interactive_source = is_interactive_space_source(plan, button);
        #[cfg(target_os = "macos")]
        let started_interactive_transition = is_interactive_source
            && self.start_interactive_space_transition(session, button, direction, action);
        #[cfg(not(target_os = "macos"))]
        let started_interactive_transition = {
            let _ = (session, button, direction, action, is_interactive_source);
            false
        };
        if started_interactive_transition {
            return;
        }
        if !self
            .outputs
            .actions
            .try_dispatch_while_pressed(&press, action)
        {
            debug!(key, %button, ?direction, "gesture press no longer active — ignored");
        }
    }

    #[cfg(target_os = "macos")]
    fn start_interactive_space_transition(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        direction: GestureDirection,
        action: &Action,
    ) -> bool {
        if !openlogi_inject::interactive_space_swipe_supported() {
            return false;
        }
        // The capture layer emits the committed aggregate as the immediately
        // following `GestureMotion` event. Create state here, then use that
        // frame as the actual Began output so no pre-threshold travel is lost.
        self.space_transitions
            .begin(session, button, direction, action)
    }

    #[cfg(target_os = "macos")]
    fn advance_interactive_space_transition(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        delta_x: i32,
    ) {
        if let Some(frame) = self.space_transitions.motion(session, button, delta_x) {
            if openlogi_inject::post_interactive_space_swipe(frame.progress_x, frame.phase) {
                return;
            }
            let fallback = matches!(frame.phase, openlogi_inject::InteractiveSpacePhase::Began)
                .then(|| self.space_transitions.fallback(session, button).cloned())
                .flatten();
            self.cancel_space_transitions(session);
            if let Some(action) = fallback
                && let Some(press) = self.gesture_presses.get(session, button)
            {
                let _ = self
                    .outputs
                    .actions
                    .try_dispatch_while_pressed(press, &action);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn end_interactive_space_transition(&mut self, session: &HidppSessionId, button: ButtonId) {
        if let Some(frame) = self.space_transitions.end(session, button) {
            let _ = openlogi_inject::post_interactive_space_swipe(frame.progress_x, frame.phase);
        }
    }

    #[cfg(target_os = "macos")]
    fn cancel_space_transitions(&mut self, session: &HidppSessionId) {
        for frame in self.space_transitions.cancel_session(session) {
            let _ = openlogi_inject::post_interactive_space_swipe(frame.progress_x, frame.phase);
        }
    }
}

#[cfg(test)]
mod tests;
