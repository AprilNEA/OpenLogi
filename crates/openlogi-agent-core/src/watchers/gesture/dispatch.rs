//! Resolve captured HID++ inputs against the active per-device plan.

mod wheel;

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU8;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, HeldWheel, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::CapturedInput;
use openlogi_hid::thumbwheel::WheelResolution;
use tracing::debug;

use self::wheel::{ScrollScale, WheelAccumulators, WheelOutput, WheelRotation};
use super::GestureOutputs;
use crate::capture_plan::DispatchPlan;
use crate::runtime::hook::SharedHookMaps;
use crate::runtime::{HidppSessionId, PointerPress, PressToken};

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

/// Correlates what happens during a held press — a completed HID++ gesture,
/// or wheel movement under a wheel-hold button — with the exact physical
/// press token admitted by the shared button runtime. The runtime remains the
/// sole authority on whether the token is still active.
#[derive(Default)]
struct HeldPresses {
    tokens: HashMap<(HidppSessionId, ButtonId), PressToken>,
}

impl HeldPresses {
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
    held_presses: HeldPresses,
    shift_held: HeldShift,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            held_presses: HeldPresses::default(),
            shift_held: HeldShift::default(),
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
        self.held_presses.get(session, button).is_some()
    }

    /// Cancel every input lifecycle retained for one capture session.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) {
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.held_presses.cancel_session(session);
        self.shift_held.0.remove(session);
        self.publish_shift(session);
    }

    /// Tell the OS hook whether `session`'s device has G-Shift held, so its
    /// gesture buttons resolve the shifted layer too.
    fn publish_shift(&self, session: &HidppSessionId) {
        let key = session.device_key();
        if let Ok(mut maps) = self.hook_maps.write() {
            if self.shift_held.active(session) {
                maps.shift_held.insert(key.to_owned());
            } else {
                maps.shift_held.remove(key);
            }
        }
    }

    /// Hold G-Shift if `button` is bound to it; `true` when it was.
    fn start_gshift(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
    ) -> bool {
        let is_shift = plan.bindings.get(&button).is_some_and(Binding::is_gshift);
        if is_shift {
            debug!(key = session.device_key(), ?button, "G-Shift held");
            self.shift_held.press(session, button);
            self.publish_shift(session);
        }
        is_shift
    }

    /// Release `button` if it is a held G-Shift button; `true` when it was.
    fn end_gshift(&mut self, session: &HidppSessionId, button: ButtonId) -> bool {
        let released = self.shift_held.release(session, button);
        if released {
            self.publish_shift(session);
        }
        released
    }

    /// `button`'s binding, from the G-Shift layer while G-Shift is held, and
    /// the pointer identity that the press carries. A gesture source has no
    /// binding: its click/swipe map owns the press.
    fn binding_for<'p>(
        &self,
        session: &HidppSessionId,
        plan: &'p DispatchPlan,
        button: ButtonId,
        is_gesture: bool,
    ) -> (Option<&'p Binding>, Option<PointerPress>) {
        let shifted = !is_gesture && self.shift_held.active(session);
        let pointer = plan.pointer.as_ref().map(|pointer| {
            if shift_binding(plan, button, shifted).is_some() {
                pointer.unscoped_press()
            } else {
                pointer.press(button)
            }
        });
        let binding = (!is_gesture)
            .then(|| press_binding(plan, button, shifted))
            .flatten();
        (binding, pointer)
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
                let Some(press) = self.held_presses.get(session, button) else {
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
                if self.start_gshift(session, plan, button) {
                    return;
                }
                // A raw-XY gesture source owns its click/swipe map; its physical
                // lifecycle is still tracked, but it must not also fire the
                // single-action projection on down.
                let is_gesture = plan.gesture_bindings.contains_key(&button)
                    || plan.side_gesture_bindings.contains_key(&button);
                let (binding, pointer) = self.binding_for(session, plan, button, is_gesture);
                let tracks_hold = is_gesture || binding.is_some_and(Binding::is_wheel_hold);
                if let Some(binding) = binding {
                    debug!(key, ?button, action = %binding.click_action().label(), "HID++ button → binding");
                } else {
                    debug!(key, ?button, "HID++ button with no binding — ignored");
                }
                let press = self
                    .outputs
                    .actions
                    .try_hidpp_button_down(session, button, binding, pointer);
                if tracks_hold {
                    if let Some(press) = press {
                        self.held_presses.start(session, button, press);
                    } else {
                        self.held_presses.end(session, button);
                    }
                }
            }
            CapturedInput::ButtonUp(button) => {
                if self.end_gshift(session, button) {
                    return;
                }
                self.outputs.actions.try_hidpp_button_up(session, button);
                self.held_presses.end(session, button);
            }
            CapturedInput::ButtonPulse(button) => self.dispatch_button_pulse(session, plan, button),
            CapturedInput::Scroll {
                increments,
                resolution,
            } => self.dispatch_wheel(session, plan, increments, resolution),
            CapturedInput::HeldWheel {
                button,
                delta,
                per_notch,
            } => self.dispatch_held_wheel(session, plan, button, delta, per_notch),
            CapturedInput::MainWheel { delta, per_notch } => self.outputs.post_main_wheel_scroll(
                session,
                main_wheel_scroll(
                    delta,
                    per_notch,
                    plan.invert_wheel,
                    openlogi_inject::natural_scrolling(),
                ),
                plan.momentum.resolve(),
            ),
            CapturedInput::ThumbwheelDirection { .. } => {
                unreachable!("thumb-wheel direction reports return before dispatch")
            }
            CapturedInput::Interrupted => self.cancel_session(session),
            CapturedInput::HostModeEntered => self.outputs.actions.host_mode_entered(key),
            CapturedInput::ButtonDiverted(_) => {
                unreachable!("the session's forwarder records diverted buttons instead")
            }
        }
    }

    /// A firmware-reported tap with no hold, such as the thumb-wheel touch
    /// sensor: fire `button`'s binding once.
    fn dispatch_button_pulse(
        &self,
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
        self.outputs.actions.dispatch_hidpp_button_pulse(
            session,
            button,
            binding,
            plan.pointer.as_ref().map(|pointer| pointer.press(button)),
        );
    }

    /// Main-wheel movement under a held wheel-hold button: scroll in its
    /// place. The press learns of the movement first, so its click is
    /// suppressed even when the scroll cannot be posted.
    fn dispatch_held_wheel(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        button: ButtonId,
        delta: i16,
        per_notch: NonZeroU8,
    ) {
        let key = session.device_key();
        let Some(press) = self.held_presses.get(session, button) else {
            debug!(key, %button, delta, "held wheel from a canceled button lifecycle — ignored");
            return;
        };
        if !self.outputs.actions.try_wheel_while_pressed(press) {
            debug!(key, %button, delta, "held wheel after the press ended — ignored");
            return;
        }
        let Some(Binding::WheelHold(binding)) = plan.bindings.get(&button) else {
            debug!(key, %button, "held wheel without a wheel-hold binding — ignored");
            return;
        };
        match binding.wheel() {
            HeldWheel::HorizontalScroll => self.outputs.post_scroll(
                session,
                held_wheel_scroll(delta, per_notch, plan.invert_wheel),
            ),
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
            WheelOutput::Scroll(delta) => {
                debug!(key, ?button, action = %action.label(), ?delta, "thumb wheel → scroll");
                self.outputs.post_scroll(session, delta);
            }
            WheelOutput::FireAction(repeats) => {
                debug!(key, ?button, action = %action.label(), repeats, "thumb wheel → action");
                for _ in 0..repeats {
                    self.outputs.actions.dispatch_pointer_action(
                        action,
                        Some(key),
                        plan.pointer.as_ref().map(|pointer| pointer.press(button)),
                    );
                }
            }
        }
    }
}

/// The horizontal scroll one held-wheel report stands for: away from the
/// user scrolls left and toward the user right, matching the thumb wheel's
/// default bindings, flipped when the device's wheel scrolls inverted (Logi
/// Options+ flips it for natural scrolling the same way).
fn held_wheel_scroll(delta: i16, per_notch: NonZeroU8, inverted: bool) -> ScrollDelta {
    let notches = f64::from(delta) / f64::from(per_notch.get());
    let rightward = if inverted { notches } else { -notches };
    ScrollDelta::wheel_ticks(rightward, 0.0)
}

/// The scroll a diverted main wheel's movement stands for: `delta` units
/// away from the user scroll up by `delta / per_notch` notches, as the wheel
/// does natively. It is reversed for the device's inversion, which the
/// wheel's native invert bit no longer applies while diverted, and for the
/// OS's natural scrolling, which macOS applies to the wheel's own events but
/// not to synthesised ones; both on cancel out.
fn main_wheel_scroll(
    delta: i16,
    per_notch: NonZeroU8,
    inverted: bool,
    natural: bool,
) -> ScrollDelta {
    let notches = f64::from(delta) / f64::from(per_notch.get());
    let upward = if inverted == natural {
        notches
    } else {
        -notches
    };
    ScrollDelta::wheel_ticks(0.0, upward)
}

/// G-Shift buttons held per session; the layer stays on until all are released.
#[derive(Default)]
struct HeldShift(HashMap<HidppSessionId, HashSet<ButtonId>>);

impl HeldShift {
    fn press(&mut self, session: &HidppSessionId, button: ButtonId) {
        self.0.entry(session.clone()).or_default().insert(button);
    }

    /// Whether `button` was a held G-Shift button.
    fn release(&mut self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.0
            .get_mut(session)
            .is_some_and(|held| held.remove(&button))
    }

    fn active(&self, session: &HidppSessionId) -> bool {
        self.0.get(session).is_some_and(|held| !held.is_empty())
    }
}

/// The held G-Shift layer's binding for `button`.
fn shift_binding(plan: &DispatchPlan, button: ButtonId, shifted: bool) -> Option<&Binding> {
    shifted.then(|| plan.shift_bindings.get(&button)).flatten()
}

fn press_binding(plan: &DispatchPlan, button: ButtonId, shifted: bool) -> Option<&Binding> {
    shift_binding(plan, button, shifted).or_else(|| plan.bindings.get(&button))
}

#[cfg(test)]
mod tests;
