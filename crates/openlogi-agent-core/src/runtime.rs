//! Shared action runtime for every background input source.
//!
//! [`ActionRuntime`] uniquely owns lifecycle resources, while cloneable
//! [`ActionDispatcher`] values let OS-hook and HID++ producers submit work
//! without owning worker shutdown. Source-specific hook interpretation lives
//! in [`hook`]; the button state machine remains an internal implementation.

mod button;
pub mod hook;

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, KeyCombo};
use openlogi_hid::{CaptureChannel, ChannelRegistry};
use tracing::{info, warn};

use self::button::{ButtonInputHandle, ButtonRuntimeEvent, ButtonRuntimeOwner, EndReason};
pub(crate) use self::button::{HidppSessionId, PressToken};
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};
use crate::receiver_access::ReceiverAccess;
use crate::{DpiCycleState, DpiCycles};

/// Output transition when an action starts within one physical press.
#[derive(Debug, PartialEq, Eq)]
enum HoldStart {
    /// The action is instantaneous and belongs to ordinary dispatch.
    NotHeld,
    /// This press newly owns one held chord.
    Started(KeyCombo),
    /// The same press changed its held action; release the old chord first.
    Replaced { old: KeyCombo, new: KeyCombo },
}

/// Held output owned by accepted press capabilities rather than by a capture
/// backend. Because every [`PressToken`] has exactly one terminal event, this
/// map gives release, cancellation, invalidation, and shutdown one path.
#[derive(Default)]
struct HeldShortcuts {
    by_press: HashMap<PressToken, KeyCombo>,
}

impl HeldShortcuts {
    fn start(&mut self, press: &PressToken, action: &Action) -> HoldStart {
        let Some(combo) = action.held_combo() else {
            return HoldStart::NotHeld;
        };
        match self.by_press.insert(press.clone(), combo.clone()) {
            Some(old) => HoldStart::Replaced {
                old,
                new: combo.clone(),
            },
            None => HoldStart::Started(combo.clone()),
        }
    }

    fn end(&mut self, press: &PressToken) -> Option<KeyCombo> {
        self.by_press.remove(press)
    }
}

#[derive(Clone)]
struct ActionExecutor {
    dpi_cycle: Arc<RwLock<DpiCycles>>,
    capture: CaptureChannel,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
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
    held: HeldShortcuts,
}

impl ButtonEventHandler {
    fn new(executor: ActionExecutor) -> Self {
        Self {
            executor,
            held: HeldShortcuts::default(),
        }
    }

    fn handle(&mut self, event: ButtonRuntimeEvent) {
        match event {
            ButtonRuntimeEvent::Started(press) => {
                if let Some(action) = press.action() {
                    self.start_action(press.token(), action, press.device_key());
                }
            }
            ButtonRuntimeEvent::Triggered { press, action } => {
                self.start_action(press.token(), &action, press.device_key());
            }
            ButtonRuntimeEvent::Ended { press, reason } => {
                if let Some(combo) = self.held.end(press.token()) {
                    openlogi_inject::release_hold(&combo);
                }
                if let EndReason::Canceled(reason) = reason {
                    info!(button = %press.button(), ?reason, "button lifecycle canceled");
                }
            }
        }
    }

    fn start_action(&mut self, press: &PressToken, action: &Action, device_key: Option<&str>) {
        match self.held.start(press, action) {
            HoldStart::NotHeld => self.executor.dispatch(action, device_key),
            HoldStart::Started(combo) => openlogi_inject::press_hold(&combo),
            HoldStart::Replaced { old, new } => {
                openlogi_inject::release_hold(&old);
                openlogi_inject::press_hold(&new);
            }
        }
    }
}

/// Runtime dependencies shared by every action source: the OS hook, HID++
/// controls, keyboard capture, and Actions Ring slot activation.
#[derive(Clone)]
pub struct ActionDispatcher {
    executor: ActionExecutor,
    buttons: ButtonInputHandle,
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
        action_ring: tokio::sync::mpsc::UnboundedSender<Option<String>>,
    ) -> io::Result<Self> {
        let executor = ActionExecutor {
            dpi_cycle,
            capture,
            registry,
            receiver_access,
            action_ring,
        };
        let mut button_handler = ButtonEventHandler::new(executor.clone());
        let buttons = ButtonRuntimeOwner::spawn(move |event| button_handler.handle(event))?;
        let input = buttons.input();
        Ok(Self {
            dispatcher: ActionDispatcher {
                executor,
                buttons: input,
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

    /// Queue one OS-hook down edge without blocking the callback. The returned
    /// token uniquely identifies this accepted press.
    pub(crate) fn try_hook_button_down(
        &self,
        button: ButtonId,
        action: Option<&Action>,
    ) -> Option<PressToken> {
        self.buttons.try_hook_down(button, action)
    }

    /// Queue one OS-hook up edge without blocking the callback.
    pub(crate) fn try_hook_button_up(&self, button: ButtonId) -> bool {
        self.buttons.try_hook_up(button)
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
        action: Option<&Action>,
    ) -> Option<PressToken> {
        self.buttons.try_hidpp_down(session, button, action)
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
        action: Option<&Action>,
    ) {
        self.buttons.try_hidpp_pulse(session, button, action);
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

    fn hold(label: &str) -> Action {
        Action::HoldShortcut(label.parse().expect("test shortcut must be valid"))
    }

    #[test]
    fn held_output_is_owned_by_the_exact_press_until_its_terminal_event() {
        let first = PressToken::hook_for_test(1, ButtonId::Back);
        let second = PressToken::hook_for_test(2, ButtonId::Forward);
        let mut held = HeldShortcuts::default();

        assert_eq!(
            held.start(&first, &hold("Ctrl+Space")),
            HoldStart::Started("Ctrl+Space".parse().expect("valid shortcut"))
        );
        assert_eq!(
            held.start(&second, &hold("Alt+F2")),
            HoldStart::Started("Alt+F2".parse().expect("valid shortcut"))
        );
        assert_eq!(
            held.end(&first),
            Some("Ctrl+Space".parse().expect("valid shortcut"))
        );
        assert_eq!(held.end(&first), None, "a press releases at most once");
        assert_eq!(
            held.end(&second),
            Some("Alt+F2".parse().expect("valid shortcut"))
        );
    }

    #[test]
    fn changing_a_hold_within_one_press_balances_the_previous_chord() {
        let press = PressToken::hook_for_test(1, ButtonId::Back);
        let mut held = HeldShortcuts::default();
        let old: KeyCombo = "Ctrl+Space".parse().expect("valid shortcut");
        let new: KeyCombo = "Alt+F2".parse().expect("valid shortcut");

        assert_eq!(
            held.start(&press, &Action::HoldShortcut(old.clone())),
            HoldStart::Started(old.clone())
        );
        assert_eq!(
            held.start(&press, &Action::HoldShortcut(new.clone())),
            HoldStart::Replaced {
                old,
                new: new.clone(),
            }
        );
        assert_eq!(held.end(&press), Some(new));
    }

    #[test]
    fn instantaneous_actions_do_not_enter_held_state() {
        let press = PressToken::hook_for_test(1, ButtonId::Back);
        let mut held = HeldShortcuts::default();

        assert_eq!(held.start(&press, &Action::Copy), HoldStart::NotHeld);
        assert_eq!(held.end(&press), None);
    }
}
