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

/// Application identity captured with a physical press and retained through
/// asynchronous button dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionDispatchTarget {
    /// A sender-less macOS browser button accepted specifically for Safari.
    // Not `expect`: production constructs this only on macOS, while the
    // cross-platform button tests construct it to verify queue retention.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(clippy::allow_attributes, reason = "see above")
    )]
    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "constructed by macOS production code and cross-platform tests"
        )
    )]
    SafariProcess(i32),
}
/// Held output owned by accepted press capabilities rather than by a capture
/// backend. Because every [`PressToken`] has exactly one terminal event, this
/// map gives release, cancellation, invalidation, shutdown, and unwinding one
/// RAII path.
#[derive(Default)]
struct HeldShortcuts {
    by_press: HashMap<PressToken, openlogi_inject::HeldChord>,
}

impl HeldShortcuts {
    fn start(&mut self, press: &PressToken, action: &Action) -> bool {
        let Some(combo) = action.held_combo() else {
            return false;
        };
        match self.by_press.entry(press.clone()) {
            std::collections::hash_map::Entry::Occupied(mut held) => {
                held.get_mut().replace(combo);
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(openlogi_inject::press_hold(combo));
            }
        }
        true
    }

    fn end(&mut self, press: &PressToken) {
        self.by_press.remove(press);
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
        self.dispatch_to(action, device_key, None);
    }

    fn dispatch_to(
        &self,
        action: &Action,
        device_key: Option<&str>,
        target: Option<ActionDispatchTarget>,
    ) {
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
            // Safari ignores synthetic browser-navigation shortcuts, so use
            // its Accessibility toolbar button. Sender-less Safari presses
            // retain their press-time PID and never fall back after that target
            // becomes stale; ordinary sources retain the keyboard fallback
            // used by Chrome and other apps. On devices where one physical
            // press is visible through both capture paths, debounce the shared
            // action before either output so the browser navigates only once.
            Action::BrowserBack | Action::BrowserForward => {
                if let Some(reservation) = browser_nav_debounce_begin(action) {
                    match target {
                        Some(ActionDispatchTarget::SafariProcess(pid)) => {
                            if !dispatch_captured_safari_navigation(
                                action,
                                pid,
                                openlogi_inject::ax_navigate_browser,
                            ) {
                                browser_nav_debounce_cancel(reservation);
                            }
                        }
                        None => dispatch_browser_navigation(
                            action,
                            openlogi_inject::ax_navigate_frontmost_browser,
                            || openlogi_inject::execute(action),
                        ),
                    }
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
                if let Some(action) = press.start_action() {
                    self.start_action(press.token(), action, press.device_key(), press.target());
                }
            }
            ButtonRuntimeEvent::Triggered { press, action } => {
                self.start_action(press.token(), &action, press.device_key(), press.target());
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

    fn start_action(
        &mut self,
        press: &PressToken,
        action: &Action,
        device_key: Option<&str>,
        target: Option<ActionDispatchTarget>,
    ) {
        if !self.held.start(press, action) {
            self.executor.dispatch_to(action, device_key, target);
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
        binding: Option<&Binding>,
        target: Option<ActionDispatchTarget>,
    ) -> Option<PressToken> {
        self.buttons
            .try_hook_down_with_target(button, binding, target)
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

/// Minimum time between two BrowserBack (or two BrowserForward) navigation
/// dispatches shared across OS-hook and HID++ capture paths.
const BROWSER_NAV_DEBOUNCE: Duration = Duration::from_millis(150);

/// Per-direction last-dispatch timestamps: `(last_back, last_forward)`.
static BROWSER_NAV_LAST: Mutex<(Option<Instant>, Option<Instant>)> = Mutex::new((None, None));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BrowserNavDebounceReservation {
    forward: bool,
    timestamp: Instant,
}

fn browser_nav_debounce_begin(action: &Action) -> Option<BrowserNavDebounceReservation> {
    let mut last = BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let forward = matches!(action, Action::BrowserForward);
    let slot = if forward { &mut last.1 } else { &mut last.0 };
    let now = Instant::now();
    let fire = slot.is_none_or(|time| now.duration_since(time) >= BROWSER_NAV_DEBOUNCE);
    if fire {
        *slot = Some(now);
        Some(BrowserNavDebounceReservation {
            forward,
            timestamp: now,
        })
    } else {
        None
    }
}

fn browser_nav_debounce_cancel(reservation: BrowserNavDebounceReservation) {
    let mut last = BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let slot = if reservation.forward {
        &mut last.1
    } else {
        &mut last.0
    };
    if *slot == Some(reservation.timestamp) {
        *slot = None;
    }
}

fn dispatch_browser_navigation(
    action: &Action,
    ax_navigate: impl FnOnce(bool) -> bool,
    keyboard_fallback: impl FnOnce(),
) {
    let forward = matches!(action, Action::BrowserForward);
    if !ax_navigate(forward) {
        keyboard_fallback();
    }
}

/// Dispatch navigation only to the Safari process captured with the physical
/// press. A failed PID-scoped lookup means the identity is stale or Safari can
/// no longer satisfy the action; never synthesize a keyboard shortcut into the
/// application that happens to be frontmost by the time this worker runs.
fn dispatch_captured_safari_navigation(
    action: &Action,
    pid: i32,
    ax_navigate: impl FnOnce(i32, bool) -> bool,
) -> bool {
    let forward = matches!(action, Action::BrowserForward);
    let navigated = ax_navigate(pid, forward);
    if !navigated {
        info!(pid, action = %action.label(), "captured Safari navigation unavailable — keyboard fallback suppressed");
    }
    navigated
}

#[cfg(test)]
mod tests {
    use super::*;

    static BROWSER_NAV_TEST_LOCK: Mutex<()> = Mutex::new(());
    #[test]
    fn instantaneous_actions_do_not_enter_held_state() {
        let press = PressToken::hook_for_test(1, ButtonId::Back);
        let mut held = HeldShortcuts::default();

        assert!(!held.start(&press, &Action::Copy));
        held.end(&press);
    }

    #[test]
    fn accessibility_browser_navigation_suppresses_the_keyboard_fallback() {
        let mut direction = None;
        let mut fell_back = false;

        dispatch_browser_navigation(
            &Action::BrowserForward,
            |forward| {
                direction = Some(forward);
                true
            },
            || fell_back = true,
        );

        assert_eq!(direction, Some(true));
        assert!(!fell_back);
    }

    #[test]
    fn failed_accessibility_navigation_falls_back_to_the_keyboard_shortcut() {
        let mut direction = None;
        let mut fell_back = false;

        dispatch_browser_navigation(
            &Action::BrowserBack,
            |forward| {
                direction = Some(forward);
                false
            },
            || fell_back = true,
        );

        assert_eq!(direction, Some(false));
        assert!(fell_back);
    }

    #[test]
    fn captured_safari_navigation_keeps_press_time_pid_and_never_falls_back() {
        let mut target = None;

        assert!(!dispatch_captured_safari_navigation(
            &Action::BrowserBack,
            417,
            |pid, forward| {
                target = Some((pid, forward));
                false
            }
        ));

        assert_eq!(target, Some((417, false)));
    }

    #[test]
    fn browser_navigation_debounce_is_per_direction_and_expires() {
        let _guard = BROWSER_NAV_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        *BROWSER_NAV_LAST
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = (Some(now), None);

        assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_none());
        assert!(browser_nav_debounce_begin(&Action::BrowserForward).is_some());

        let expired = Instant::now()
            .checked_sub(BROWSER_NAV_DEBOUNCE)
            .expect("the debounce interval fits before the current instant");
        BROWSER_NAV_LAST
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0 = Some(expired);

        assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_some());
    }

    #[test]
    fn failed_captured_navigation_releases_its_debounce_reservation() {
        let _guard = BROWSER_NAV_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *BROWSER_NAV_LAST
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = (None, None);
        let reservation = browser_nav_debounce_begin(&Action::BrowserBack)
            .expect("the first attempt should reserve the direction");

        assert!(!dispatch_captured_safari_navigation(
            &Action::BrowserBack,
            417,
            |_, _| false
        ));
        browser_nav_debounce_cancel(reservation);

        assert!(browser_nav_debounce_begin(&Action::BrowserBack).is_some());
    }

    #[test]
    fn canceling_a_failed_attempt_never_clears_a_newer_reservation() {
        let _guard = BROWSER_NAV_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let original = Instant::now();
        let newer = original + Duration::from_millis(1);
        *BROWSER_NAV_LAST
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = (Some(newer), None);
        browser_nav_debounce_cancel(BrowserNavDebounceReservation {
            forward: false,
            timestamp: original,
        });

        assert_eq!(
            BROWSER_NAV_LAST
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .0,
            Some(newer)
        );
    }
}
