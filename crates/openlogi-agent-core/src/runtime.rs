//! Shared action runtime for every background input source.
//!
//! [`ActionRuntime`] uniquely owns lifecycle resources, while cloneable
//! [`ActionDispatcher`] values let OS-hook and HID++ producers submit work
//! without owning worker shutdown. Source-specific hook interpretation lives
//! in [`hook`]; the button state machine remains an internal implementation.

mod button;
pub mod hook;
pub mod scroll;

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_hid::{CaptureChannel, ChannelRegistry, DeviceIoGate};
use tracing::{info, warn};

use self::button::{
    ButtonInputHandle, ButtonRuntimeEvent, ButtonRuntimeOwner, EndReason, PressControl,
};
pub(crate) use self::button::{HidppSessionId, PressToken};
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};
use crate::receiver_access::ReceiverAccess;
use crate::{DpiCycleState, DpiCycles};

/// Cloneable read-mostly horizontal-scroll hold state. The OS-hook callback
/// cannot consistently map a wheel event to a stable config key on every
/// platform, so any accepted live hold enables the modifier for wheel sources
/// that have already passed the hook's Logitech/non-trackpad safety policy.
#[derive(Clone, Default)]
struct HorizontalScrollState {
    active: Arc<AtomicBool>,
}

impl HorizontalScrollState {
    fn publish(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

enum HeldOutput {
    Chord { _held: openlogi_inject::HeldChord },
    HorizontalScroll,
}

/// Held outputs owned by accepted press capabilities rather than by a capture
/// backend. Because every [`PressToken`] has exactly one terminal event, this
/// map gives release, cancellation, invalidation, shutdown, and unwinding one
/// RAII path.
struct HeldOutputs {
    by_press: HashMap<PressToken, HeldOutput>,
    horizontal_scroll: HorizontalScrollState,
}

impl HeldOutputs {
    fn new(horizontal_scroll: HorizontalScrollState) -> Self {
        Self {
            by_press: HashMap::new(),
            horizontal_scroll,
        }
    }

    fn start(&mut self, press: &PressToken, action: &Action) -> bool {
        let output = match action {
            Action::HoldShortcut(combo) => HeldOutput::Chord {
                _held: openlogi_inject::press_hold(combo),
            },
            Action::HorizontalScroll => HeldOutput::HorizontalScroll,
            _ => return false,
        };
        self.by_press.insert(press.clone(), output);
        self.publish_horizontal_scroll();
        true
    }

    fn end(&mut self, press: &PressToken) {
        if self.by_press.remove(press).is_some() {
            self.publish_horizontal_scroll();
        }
    }

    fn publish_horizontal_scroll(&self) {
        self.horizontal_scroll.publish(
            self.by_press
                .values()
                .any(|output| matches!(output, HeldOutput::HorizontalScroll)),
        );
    }
}

impl Drop for HeldOutputs {
    fn drop(&mut self) {
        self.horizontal_scroll.publish(false);
    }
}

#[derive(Clone)]
struct ActionExecutor {
    dpi_cycle: Arc<RwLock<DpiCycles>>,
    capture: CaptureChannel,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
    action_ring: tokio::sync::mpsc::UnboundedSender<Option<String>>,
}

impl ActionExecutor {
    fn dispatch(&self, action: &Action, device_key: Option<&str>) {
        if matches!(action, Action::ShowActionsRing) {
            if self
                .action_ring
                .send(device_key.map(str::to_owned))
                .is_err()
            {
                warn!("Actions Ring runtime unavailable — trigger ignored");
            }
            return;
        }

        let next = match action {
            Action::CycleDpiPresets => match self.dpi_cycle.write() {
                Ok(mut guard) => guard.state_for(device_key).and_then(DpiCycleState::cycle),
                Err(e) => {
                    warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                    None
                }
            },
            Action::SetDpiPreset(i) => match self.dpi_cycle.write() {
                Ok(mut guard) => guard
                    .state_for(device_key)
                    .and_then(|state| state.set(usize::from(*i))),
                Err(e) => {
                    warn!(error = %e, "dpi_cycle lock poisoned — set skipped");
                    None
                }
            },
            Action::ToggleSmartShift => {
                let target = self
                    .dpi_cycle
                    .read()
                    .ok()
                    .and_then(|cycles| cycles.target_for(device_key));
                info!("SmartShift toggle → flipping wheel mode");
                toggle_smartshift_in_background(
                    &self.capture,
                    &self.registry,
                    &self.receiver_access,
                    &self.device_io,
                    target,
                );
                return;
            }
            // BrowserBack/BrowserForward fall through to the keyboard shortcut
            // (Cmd+[ / Cmd+]) here — for Chrome and other apps that respond to
            // it, and as the HID++ gesture watcher's own fallback when its
            // AXPress attempt (Safari) fails. On devices where one physical
            // press is visible through both capture paths, debounce the shared
            // action so the browser navigates only once.
            Action::BrowserBack | Action::BrowserForward => {
                if browser_nav_debounce_ok(action) {
                    openlogi_inject::execute(action);
                } else {
                    info!(action = %action.label(), "browser nav debounced — duplicate dispatch path suppressed");
                }
                None
            }
            other => {
                openlogi_inject::execute(other);
                None
            }
        };
        if let Some((dpi, target)) = next {
            info!(%dpi, "DPI action → writing to device");
            write_dpi_in_background(
                &self.capture,
                &self.registry,
                &self.receiver_access,
                &self.device_io,
                target,
                dpi,
            );
        } else if matches!(action, Action::CycleDpiPresets | Action::SetDpiPreset(_)) {
            info!(
                action = %action.label(),
                "no DPI presets configured for active device — press ignored"
            );
        }
    }
}

struct ButtonEventHandler {
    executor: ActionExecutor,
    held: HeldOutputs,
}

impl ButtonEventHandler {
    fn new(executor: ActionExecutor, horizontal_scroll: HorizontalScrollState) -> Self {
        Self {
            executor,
            held: HeldOutputs::new(horizontal_scroll),
        }
    }

    fn handle(&mut self, event: ButtonRuntimeEvent) {
        match event {
            ButtonRuntimeEvent::Started(press) => {
                if let Some(action) = press.start_action() {
                    self.start_action(press.token(), action, press.device_key());
                }
            }
            ButtonRuntimeEvent::Triggered { press, action } => {
                self.start_action(press.token(), &action, press.device_key());
            }
            ButtonRuntimeEvent::Ended { press, reason } => {
                self.held.end(press.token());
                if let EndReason::Canceled(reason) = reason {
                    match press.control() {
                        PressControl::Button(button) => {
                            info!(button = %button, ?reason, "button lifecycle canceled");
                        }
                        PressControl::Key(keycode) => {
                            info!(keycode, ?reason, "key lifecycle canceled");
                        }
                    }
                }
            }
        }
    }

    fn start_action(&mut self, press: &PressToken, action: &Action, device_key: Option<&str>) {
        if !self.held.start(press, action) {
            self.executor.dispatch(action, device_key);
        }
    }
}

/// Runtime dependencies shared by every action source: the OS hook, HID++
/// controls, keyboard capture, and Actions Ring slot activation.
#[derive(Clone)]
pub struct ActionDispatcher {
    executor: ActionExecutor,
    buttons: ButtonInputHandle,
    horizontal_scroll: HorizontalScrollState,
}

/// Unique owner of the button worker plus its cloneable action dispatcher.
///
/// Keep this value in the agent's main runtime so graceful shutdown can stop
/// and join the worker after capture sources have stopped producing input.
pub struct ActionRuntime {
    dispatcher: ActionDispatcher,
    buttons: ButtonRuntimeOwner,
}

impl ActionRuntime {
    /// Build the action executor and its source-independent button worker.
    pub fn new(
        dpi_cycle: Arc<RwLock<DpiCycles>>,
        capture: CaptureChannel,
        registry: ChannelRegistry,
        receiver_access: ReceiverAccess,
        device_io: DeviceIoGate,
        action_ring: tokio::sync::mpsc::UnboundedSender<Option<String>>,
    ) -> io::Result<Self> {
        let executor = ActionExecutor {
            dpi_cycle,
            capture,
            registry,
            receiver_access,
            device_io,
            action_ring,
        };
        let horizontal_scroll = HorizontalScrollState::default();
        let mut button_handler =
            ButtonEventHandler::new(executor.clone(), horizontal_scroll.clone());
        let buttons = ButtonRuntimeOwner::spawn(move |event| button_handler.handle(event))?;
        let input = buttons.input();
        Ok(Self {
            dispatcher: ActionDispatcher {
                executor,
                buttons: input,
                horizontal_scroll,
            },
            buttons,
        })
    }

    /// Clone the non-owning dispatcher for hooks, watchers, and the IPC server.
    #[must_use]
    pub fn dispatcher(&self) -> ActionDispatcher {
        self.dispatcher.clone()
    }

    /// Reject new button input, emit terminal cancellation, and join the worker.
    pub fn shutdown(&mut self) {
        let _ = self.buttons.shutdown();
    }
}

impl ActionDispatcher {
    /// Route one action without blocking the input callback.
    pub fn dispatch(&self, action: &Action, device_key: Option<&str>) {
        self.executor.dispatch(action, device_key);
    }

    /// Whether any accepted live press currently owns the
    /// vertical-to-horizontal main-wheel modifier.
    pub(crate) fn horizontal_scroll_active(&self) -> bool {
        self.horizontal_scroll.active()
    }

    /// Queue one OS-hook down edge without blocking the callback. The returned
    /// token uniquely identifies this accepted press.
    pub(crate) fn try_hook_button_down(
        &self,
        button: ButtonId,
        binding: Option<&Binding>,
    ) -> Option<PressToken> {
        self.buttons.try_hook_down(button, binding)
    }

    /// Queue one OS-hook up edge without blocking the callback.
    pub(crate) fn try_hook_button_up(&self, button: ButtonId) -> bool {
        self.buttons.try_hook_up(button)
    }

    /// Queue one function-key down edge without blocking the hook callback.
    pub(crate) fn try_hook_key_down(&self, keycode: u16, action: &Action) -> bool {
        self.buttons.try_hook_key_down(keycode, action).is_some()
    }

    /// Queue one function-key up edge without blocking the hook callback.
    pub(crate) fn try_hook_key_up(&self, keycode: u16) -> bool {
        self.buttons.try_hook_key_up(keycode)
    }

    /// Execute a semantic gesture action only if its exact press is still live.
    pub(crate) fn try_dispatch_while_pressed(&self, press: &PressToken, action: &Action) -> bool {
        self.buttons.try_trigger_while_pressed(press, action)
    }

    /// End a gesture hold whose release was lost before another button takes
    /// over the thread-local gesture accumulator.
    fn cancel_stale_hook_press(&self, press: &PressToken) {
        self.buttons.cancel_stale_press(press);
    }

    /// Cancel every active press owned by the current OS-hook callback thread.
    /// This is the terminal edge for
    /// [`openlogi_hook::MouseEvent::CaptureInterrupted`].
    pub(crate) fn cancel_hook_thread_buttons(&self) {
        self.buttons.cancel_hook_thread();
    }

    /// Queue one HID++ down edge for a specific capture session.
    pub(crate) fn try_hidpp_button_down(
        &self,
        session: &HidppSessionId,
        button: ButtonId,
        binding: Option<&Binding>,
    ) -> Option<PressToken> {
        self.buttons.try_hidpp_down(session, button, binding)
    }

    /// Queue one HID++ up edge for a specific capture session.
    pub(crate) fn try_hidpp_button_up(&self, session: &HidppSessionId, button: ButtonId) -> bool {
        self.buttons.try_hidpp_up(session, button)
    }

    /// Deliver an instantaneous HID++ button tap as one balanced lifecycle.
    /// Used only for firmware reports that expose no physical release edge.
    pub(crate) fn dispatch_hidpp_button_pulse(
        &self,
        session: &HidppSessionId,
        button: ButtonId,
        binding: Option<&Binding>,
    ) {
        self.buttons.try_hidpp_pulse(session, button, binding);
    }

    /// Cancel presses from a HID++ session that is stopping or has died.
    pub(crate) fn cancel_hidpp_session(&self, session: &HidppSessionId) {
        self.buttons.cancel_hidpp_session(session);
    }

    /// Invalidate every active lifecycle after a binding/profile change or
    /// capture-owner transition. Events already queued under the old
    /// generation are ignored even if they arrive after this call's wake-up.
    pub fn cancel_all_buttons(&self) {
        self.buttons.invalidate_all();
    }

    /// Cancel only presses owned by an OS-hook callback. HID++ capture does not
    /// depend on Accessibility and remains active when the native hook stops.
    pub fn cancel_hook_buttons(&self) {
        self.buttons.cancel_hooks();
    }
}

/// Minimum time between two BrowserBack (or two BrowserForward) keyboard
/// dispatches shared across OS-hook and HID++ capture paths.
const BROWSER_NAV_DEBOUNCE: Duration = Duration::from_millis(150);

/// Per-direction last-dispatch timestamps: `(last_back, last_forward)`.
static BROWSER_NAV_LAST: Mutex<(Option<Instant>, Option<Instant>)> = Mutex::new((None, None));

fn browser_nav_debounce_ok(action: &Action) -> bool {
    let mut last = BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let slot = if matches!(action, Action::BrowserForward) {
        &mut last.1
    } else {
        &mut last.0
    };
    let now = Instant::now();
    let fire = slot.is_none_or(|time| now.duration_since(time) >= BROWSER_NAV_DEBOUNCE);
    if fire {
        *slot = Some(now);
    }
    fire
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instantaneous_actions_do_not_enter_held_state() {
        let press = PressToken::hook_for_test(1, ButtonId::Back);
        let mut held = HeldOutputs::new(HorizontalScrollState::default());

        assert!(!held.start(&press, &Action::Copy));
        held.end(&press);
    }

    #[test]
    fn horizontal_scroll_holds_are_selection_independent_and_balanced() {
        let state = HorizontalScrollState::default();
        let mut held = HeldOutputs::new(state.clone());
        let first = PressToken::hidpp_for_test(1, "mouse-a", 7, ButtonId::DpiToggle);
        let second = PressToken::hidpp_for_test(2, "mouse-b", 3, ButtonId::DpiToggle);

        assert!(!state.active());
        assert!(held.start(&first, &Action::HorizontalScroll));
        assert!(state.active());

        assert!(held.start(&second, &Action::HorizontalScroll));
        assert!(state.active());

        held.end(&first);
        assert!(state.active());

        held.end(&second);
        assert!(!state.active());

        assert!(held.start(&first, &Action::HorizontalScroll));
        drop(held);
        assert!(!state.active());
    }
}
