//! Resolve captured HID++ inputs against the active per-device plan.

mod wheel;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use openlogi_core::binding::{Action, Binding, ButtonId, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_hid::{CapturedInput, DeviceRoute};
use tokio::task::JoinHandle;
use tracing::debug;

use self::wheel::{ScrollScale, WheelAccumulators, WheelOutput, WheelRotation};
use super::GestureOutputs;
use crate::capture_plan::DispatchPlan;
use crate::hardware::{SpyNativeDpi, SpyShiftHold};
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

/// In-flight software DPI writes for unbound G6–G8. Finished handles are
/// dropped on the next track so a long-lived capture session cannot grow the
/// vector without bound; cancel still awaits whatever is still running.
#[derive(Default)]
struct SpyOps(HashMap<HidppSessionId, Vec<JoinHandle<()>>>);

impl SpyOps {
    fn track(&mut self, session: &HidppSessionId, handle: JoinHandle<()>) {
        let ops = self.0.entry(session.clone()).or_default();
        ops.retain(|handle| !handle.is_finished());
        ops.push(handle);
    }

    async fn cancel_session(&mut self, session: &HidppSessionId) {
        if let Some(ops) = self.0.remove(session) {
            for handle in ops {
                let _ = handle.await;
            }
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
    /// DPI to restore when an unbound G6 (DPI Shift) is released.
    spy_shift: Arc<tokio::sync::Mutex<HashMap<HidppSessionId, SpyShiftHold>>>,
    /// In-flight software DPI writes for unbound G6–G8. Cancel awaits these
    /// before restoring so shutdown cannot drop a ShiftDown or ShiftUp.
    spy_ops: SpyOps,
}

impl InputDispatcher {
    /// Build a dispatcher for session-owned capture-plan snapshots.
    pub(super) fn new(outputs: GestureOutputs) -> Self {
        Self {
            hook_maps: outputs.hook_maps.clone(),
            outputs,
            wheels: SessionWheels::default(),
            gesture_presses: GesturePresses::default(),
            spy_shift: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            spy_ops: SpyOps::default(),
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
    /// Awaits in-flight G6–G8 software DPI writes and any saved G6 restore
    /// before returning, so teardown and a replacement session cannot race it.
    pub(super) async fn cancel_session(&mut self, session: &HidppSessionId) {
        self.outputs.cancel_session(session);
        self.wheels.cancel_session(session);
        self.gesture_presses.cancel_session(session);
        self.spy_ops.cancel_session(session).await;
        self.outputs
            .actions
            .restore_spy_shift(session.clone(), Arc::clone(&self.spy_shift))
            .await;
    }

    fn spawn_spy_native_dpi(
        &mut self,
        session: &HidppSessionId,
        route: DeviceRoute,
        kind: SpyNativeDpi,
    ) {
        let handle = self.outputs.actions.spawn_spy_native_dpi(
            session.clone(),
            route,
            kind,
            Arc::clone(&self.spy_shift),
        );
        self.spy_ops.track(session, handle);
    }

    /// Route one captured input from `session` to its bound action or
    /// re-synthesised scroll output.
    pub(super) fn dispatch(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        route: &DeviceRoute,
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
                self.dispatch_button_down(session, plan, route, key, button);
            }
            CapturedInput::ButtonUp(button) => {
                self.dispatch_button_up(session, plan, route, button);
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

    fn dispatch_button_down(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        route: &DeviceRoute,
        key: &str,
        button: ButtonId,
    ) {
        // A raw-XY gesture source owns its click/swipe map; its physical
        // lifecycle is still tracked, but it must not also fire the
        // single-action projection on down.
        let is_gesture = plan.gesture_bindings.contains_key(&button)
            || plan.side_gesture_bindings.contains_key(&button);
        let binding = (!is_gesture).then(|| plan.bindings.get(&button)).flatten();
        let customized = binding.is_some_and(Binding::has_configured_action);
        if customized {
            if let Some(binding) = binding {
                debug!(key, ?button, action = %binding.click_action().label(), "HID++ button → binding");
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
        } else if let Some(kind) = spy_native_kind(button, true) {
            debug!(key, ?button, "unbound spy button → software DPI");
            self.spawn_spy_native_dpi(session, route.clone(), kind);
        } else {
            debug!(key, ?button, "HID++ button with no binding — ignored");
        }
    }

    fn dispatch_button_up(
        &mut self,
        session: &HidppSessionId,
        plan: &DispatchPlan,
        route: &DeviceRoute,
        button: ButtonId,
    ) {
        self.outputs.actions.try_hidpp_button_up(session, button);
        self.gesture_presses.end(session, button);
        if !plan
            .bindings
            .get(&button)
            .is_some_and(Binding::has_configured_action)
            && let Some(kind) = spy_native_kind(button, false)
        {
            self.spawn_spy_native_dpi(session, route.clone(), kind);
        }
    }
}

fn spy_native_kind(button: ButtonId, down: bool) -> Option<SpyNativeDpi> {
    match (button, down) {
        (ButtonId::DpiUp, true) => Some(SpyNativeDpi::Step { up: true }),
        (ButtonId::DpiDown, true) => Some(SpyNativeDpi::Step { up: false }),
        (ButtonId::DpiShift, true) => Some(SpyNativeDpi::ShiftDown),
        (ButtonId::DpiShift, false) => Some(SpyNativeDpi::ShiftUp),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
