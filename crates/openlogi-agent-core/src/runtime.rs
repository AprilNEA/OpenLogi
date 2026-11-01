//! Shared action runtime for every background input source.
//!
//! [`ActionRuntime`] uniquely owns lifecycle resources, while cloneable
//! [`ActionDispatcher`] values let OS-hook and HID++ producers submit work
//! without owning worker shutdown. Source-specific hook interpretation lives
//! in [`hook`]; the button state machine remains an internal implementation.

mod button;
pub mod hook;
mod pointer;
pub mod scroll;

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError, RwLock, mpsc};
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, Binding, ButtonId};
use tracing::{debug, info, warn};

use self::button::{
    ButtonInputHandle, ButtonRuntimeEvent, ButtonRuntimeOwner, EndReason, PressControl,
};
pub(crate) use self::button::{HidppSessionId, PressToken};
pub(crate) use self::pointer::{FocusBinding, PointerPress, PointerSelection};
use crate::dpi::DpiBeforeShift;
use crate::hardware::{
    DeviceAccess, restore_dpi_in_background, step_dpi_in_background,
    toggle_smartshift_in_background, write_pointer_speed_in_background,
};
use crate::pointer_speed::{PrecisionPress, Restore, ShiftRestore, SpeedPlace, SpeedWrite};
use crate::{DpiCycleState, DpiCycles, PointerSpeeds};
use openlogi_hid::{DeviceRoute, Dpi};

/// Wait for a runtime worker's next command, or until its earliest deadline.
///
/// `None` blocks without a timeout, so an idle worker never wakes on its own:
/// whoever changes its state out of band (shutdown, generation invalidation)
/// sends an explicit wake command after publishing that change. A deadline
/// that has already passed returns without blocking.
fn receive_command<T>(
    commands: &mpsc::Receiver<T>,
    deadline: Option<Instant>,
) -> Result<T, mpsc::RecvTimeoutError> {
    deadline.map_or_else(
        || {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        },
        |deadline| commands.recv_timeout(deadline.saturating_duration_since(Instant::now())),
    )
}

/// Which way a keypad page action turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTurn {
    /// [`Action::PreviousKeypadPage`].
    Previous,
    /// [`Action::NextKeypadPage`].
    Next,
}

/// An action the agent itself carries out rather than the OS injector,
/// handed from the dispatcher to the agent's lifecycle loop, which owns the
/// state it changes. Each carries the config key of the device it was pressed
/// on, when the source knows one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRequest {
    /// Open the Actions Ring ([`Action::ShowActionsRing`]).
    ShowActionsRing {
        /// The pressing device.
        device_key: Option<String>,
    },
    /// Turn the pressing keypad's binding page.
    TurnKeypadPage {
        /// The pressing device.
        device_key: Option<String>,
        /// Which way.
        turn: PageTurn,
    },
}

/// Application identity captured with a physical press and retained through
/// asynchronous button dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionDispatchTarget {
    /// Safari's Accessibility target at the instant the physical press arrived.
    SafariProcess(i32),
    /// The ordinary browser-navigation shortcut target captured outside Safari.
    Keyboard,
    /// The pointer context that selected the binding. Validated off the tap
    /// before output. Keystrokes from a binding that the focused application
    /// shares go where focused mode sends them. Other keystrokes need the
    /// identified hovered window to have focus. A shared binding and an
    /// unidentified target retain the frontmost Safari captured with the
    /// press, as focused mode does.
    Pointer {
        target: openlogi_hook::PointerTarget,
        focus: FocusBinding,
        fallback_safari_pid: Option<i32>,
    },
}

impl ActionDispatchTarget {
    fn capture() -> Self {
        Self::for_pointer(None, openlogi_hook::frontmost_safari_pid)
    }

    fn for_pointer(
        press: Option<PointerPress>,
        capture_safari_pid: impl FnOnce() -> Option<i32>,
    ) -> Self {
        // Focused mode, a shared binding, and an unidentified target send
        // Safari navigation where focus is at the press.
        let safari_pid = press
            .is_none_or(|press| {
                press.target == openlogi_hook::PointerTarget::Unavailable
                    || press.focus == FocusBinding::Shared
            })
            .then(capture_safari_pid)
            .flatten();
        match press {
            Some(PointerPress { target, focus }) => Self::Pointer {
                target,
                focus,
                fallback_safari_pid: safari_pid,
            },
            None => safari_pid.map_or(Self::Keyboard, Self::SafariProcess),
        }
    }
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
        let Some((combo, keys)) = action.held_chord() else {
            return false;
        };
        match self.by_press.entry(press.clone()) {
            std::collections::hash_map::Entry::Occupied(mut held) => {
                held.get_mut().replace(combo, keys);
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(openlogi_inject::press_hold(combo, keys));
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
    pointer_speeds: Arc<PointerSpeeds>,
    access: DeviceAccess,
    requests: tokio::sync::mpsc::UnboundedSender<ActionRequest>,
}

/// The request the agent handles itself for `action` pressed on
/// `device_key`, or `None` for an action the executor runs.
///
/// The ring is drawn by OpenLogi at the cursor and acts on no window, so no
/// pointer target gates it. It must not: while the ring is showing, the
/// pointer is over the ring's own floating window, which classifies as
/// `PointerTarget::Unavailable`, and a gated second trigger press could never
/// close the ring. A keypad page turn acts on the keypad alone, so no window
/// gates it either.
fn agent_request(action: &Action, device_key: Option<&str>) -> Option<ActionRequest> {
    let device_key = device_key.map(str::to_owned);
    match action {
        Action::ShowActionsRing => Some(ActionRequest::ShowActionsRing { device_key }),
        Action::PreviousKeypadPage => Some(ActionRequest::TurnKeypadPage {
            device_key,
            turn: PageTurn::Previous,
        }),
        Action::NextKeypadPage => Some(ActionRequest::TurnKeypadPage {
            device_key,
            turn: PageTurn::Next,
        }),
        _ => None,
    }
}

impl ActionExecutor {
    fn dispatch(&self, action: &Action, device_key: Option<&str>) {
        self.dispatch_to(action, device_key, ActionDispatchTarget::capture());
    }

    /// Turn precision mode on or off for the mouse `device_key` names, or
    /// the selected device, and write the speed that follows. Returns
    /// `false` when that device is not an online mouse that `press` acts on.
    fn toggle_precision_mode(&self, device_key: Option<&str>, press: PrecisionPress) -> bool {
        let Some((on, write)) = self.pointer_speeds.toggle_precision(device_key, press) else {
            return false;
        };
        info!(on, target = ?write.target, "precision mode toggled → writing pointer speed");
        self.write_speed(write);
        true
    }

    /// Write `write` in the background, and settle the restore it carries
    /// once it lands.
    fn write_speed(&self, write: SpeedWrite) {
        self.write_speed_reading(write, None);
    }

    /// [`Self::write_speed`], first reading the sensor DPI into `before`.
    fn write_speed_reading(&self, write: SpeedWrite, before: Option<DpiBeforeShift>) {
        self.write_speed_then(write, before, || {});
    }

    /// Write the DPI a preset action picked. If the mouse rejects it, the
    /// cache forgets that DPI, so a DPI shift reads the mouse instead.
    fn write_preset(&self, route: &DeviceRoute, dpi: Dpi) {
        let rejected = self.preset_rejected(route.clone());
        self.write_speed_then(
            self.pointer_speeds.preset_write(route, dpi),
            None,
            move || {
                rejected(dpi);
            },
        );
    }

    /// What a preset write the mouse at `route` rejected does with its DPI:
    /// the mouse kept its old DPI, so the cache forgets the rejected one,
    /// unless a newer choice has replaced it meanwhile.
    fn preset_rejected(&self, route: DeviceRoute) -> impl FnOnce(Dpi) + Send + 'static {
        let cycles = Arc::clone(&self.dpi_cycle);
        move |dpi| match cycles.write() {
            Ok(mut cycles) => cycles.rejected_at(&route, dpi),
            Err(e) => warn!(error = %e, "dpi_cycle lock poisoned — rejected DPI kept"),
        }
    }

    /// [`Self::write_speed_reading`], running `on_failed` when the mouse
    /// rejects the write or it times out.
    fn write_speed_then(
        &self,
        write: SpeedWrite,
        before: Option<DpiBeforeShift>,
        on_failed: impl FnOnce() + Send + 'static,
    ) {
        let speeds = Arc::clone(&self.pointer_speeds);
        let SpeedWrite {
            route,
            target,
            ticket,
            restore,
            led,
        } = write;
        write_pointer_speed_in_background(
            self.access.op(&route),
            ticket,
            target,
            led,
            before,
            move || {
                if let Some(restore) = restore {
                    speeds.restored(&restore);
                }
            },
            on_failed,
        );
    }

    /// `device_key`'s session entered host mode, which may have loaded the
    /// stored profile's DPI: write the saved one again, as a reconnect does.
    fn host_mode_entered(&self, device_key: &str) {
        let Some(write) = self.pointer_speeds.host_mode_write(device_key) else {
            return;
        };
        if let Some(dpi) = write.dpi()
            && let Ok(mut cycles) = self.dpi_cycle.write()
        {
            cycles.reapplied(device_key, dpi);
        }
        info!(target = ?write.target, "host mode entered → writing the saved DPI");
        self.write_speed(write);
    }

    /// Step `device_key`'s DPI presets one up for [`Action::NextDpiPreset`],
    /// else one down, without wrapping.
    fn step_dpi(
        &self,
        device_key: Option<&str>,
        action: &Action,
    ) -> Option<(Dpi, Option<DeviceRoute>)> {
        match self.dpi_cycle.write() {
            Ok(mut guard) => guard.state_for(device_key).and_then(|state| {
                if matches!(action, Action::NextDpiPreset) {
                    state.step_up()
                } else {
                    state.step_down()
                }
            }),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — step skipped");
                None
            }
        }
    }

    /// [`Self::step_dpi`] for a mouse whose DPI OpenLogi never set: its
    /// preset index says nothing about where it is, so the write reads the
    /// mouse first and steps from that. `false` when the DPI is known, or
    /// there is no preset or mouse to step, so the plain step runs.
    fn step_dpi_from_reading(&self, device_key: Option<&str>, action: &Action) -> bool {
        let unknown = self.dpi_cycle.read().ok().and_then(|cycles| {
            let key = device_key.or(cycles.selected.as_deref())?;
            let state = cycles.by_key.get(key)?;
            let route = state.target.clone()?;
            (state.current.is_none() && !state.presets.is_empty()).then(|| (key.to_owned(), route))
        });
        let Some((key, route)) = unknown else {
            return false;
        };
        let step = if matches!(action, Action::NextDpiPreset) {
            DpiCycleState::step_up_from
        } else {
            DpiCycleState::step_down_from
        };
        info!("DPI preset step on a DPI OpenLogi never set → reading the mouse first");
        let SpeedPlace {
            route,
            ticket,
            restore,
            led,
        } = self.pointer_speeds.preset_place(&route);
        let cycles = Arc::clone(&self.dpi_cycle);
        let speeds = Arc::clone(&self.pointer_speeds);
        step_dpi_in_background(
            self.access.op(&route),
            ticket,
            led,
            move |reading| {
                let mut cycles = cycles.write().ok()?;
                step(cycles.by_key.get_mut(&key)?, reading).map(|(dpi, _)| dpi)
            },
            move || {
                if let Some(restore) = restore {
                    speeds.restored(&restore);
                }
            },
            self.preset_rejected(route.clone()),
        );
        true
    }

    fn dispatch_to(&self, action: &Action, device_key: Option<&str>, target: ActionDispatchTarget) {
        if let Some(request) = agent_request(action, device_key) {
            if self.requests.send(request).is_err() {
                warn!(action = %action.label(), "agent runtime unavailable — request ignored");
            }
            return;
        }
        let Some(target) = target.resolve(action) else {
            return;
        };

        let next = match action {
            Action::CycleDpiPresets => {
                // A mouse that scales its pointer with `0x2205` has no
                // adjustable DPI. Its DPI button is the precision-mode button
                // (MX Ergo CID 0xED), which Logi Options+ binds to precision
                // mode, so the default binding switches between the saved
                // speed and the precision speed (#1372). This check comes
                // before the presets: such a mouse can still have presets
                // saved by an older panel or by hand, and the mouse would
                // refuse those DPI writes.
                if self.toggle_precision_mode(device_key, PrecisionPress::DpiButton) {
                    return;
                }
                match self.dpi_cycle.write() {
                    Ok(mut guard) => guard.state_for(device_key).and_then(DpiCycleState::cycle),
                    Err(e) => {
                        warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                        None
                    }
                }
            }
            Action::NextDpiPreset | Action::PreviousDpiPreset => {
                if self.step_dpi_from_reading(device_key, action) {
                    return;
                }
                self.step_dpi(device_key, action)
            }
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
                if let Some(target) = target {
                    info!("SmartShift toggle → flipping wheel mode");
                    toggle_smartshift_in_background(self.access.op(&target));
                } else {
                    debug!("no target device — SmartShift toggle skipped");
                }
                return;
            }
            Action::TogglePrecisionMode => {
                if !self.toggle_precision_mode(device_key, PrecisionPress::Assigned) {
                    debug!("no online mouse with pointer scaling — precision toggle skipped");
                }
                return;
            }
            // Browser navigation uses Safari's captured Accessibility target
            // or, elsewhere, the platform shortcut (on macOS pressed as the
            // frontmost app's menu item), not a native mouse click. Preserve
            // the press-time target through queued dispatch and debounce
            // duplicate capture paths before either output.
            Action::BrowserBack | Action::BrowserForward => {
                if let Some(reservation) = browser_nav_debounce_begin(action) {
                    if dispatch_browser_navigation(
                        action,
                        target,
                        openlogi_inject::ax_navigate_browser,
                        || openlogi_inject::execute(action),
                    ) {
                        browser_nav_debounce_commit(reservation);
                    } else {
                        browser_nav_debounce_cancel(reservation);
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
        self.write_dpi_action(action, next);
    }

    /// Write the DPI a preset action picked, `next`; `None` when it had no
    /// preset to pick.
    fn write_dpi_action(&self, action: &Action, next: Option<(Dpi, Option<DeviceRoute>)>) {
        if let Some((dpi, target)) = next {
            // No target: a dev environment without a real device.
            if let Some(target) = target {
                info!(%dpi, "DPI action → writing to device");
                // Numbered with the pointer-speed writes: on a DPI mouse it
                // ends precision mode, and an older precision write must not
                // land after it.
                self.write_preset(&target, dpi);
            } else {
                debug!(%dpi, "no target device — DPI write skipped");
            }
        } else if matches!(
            action,
            Action::CycleDpiPresets
                | Action::SetDpiPreset(_)
                | Action::NextDpiPreset
                | Action::PreviousDpiPreset
        ) {
            info!(
                action = %action.label(),
                "no DPI presets configured for active device — press ignored"
            );
        }
    }
}

/// Log a bound action the OS input hook captured, from the worker that
/// dispatches it. The hook's callback logs nothing on this path: the file
/// appender's lock and its daily rollover must not run inside the callback
/// the hook watchdog guards (#2096).
fn log_bound_action(control: &PressControl, action: &Action) {
    match control {
        PressControl::Button(button) => {
            info!(button = %button, action = %action.label(), "button → executing bound action");
        }
        PressControl::Key(keycode) => {
            info!(keycode, action = %action.label(), "key → executing bound action");
        }
    }
}

/// Held DPI-shift presses per device.
///
/// Releasing the last one returns to the DPI config or an action last set, so
/// a preset picked during the hold stays. When OpenLogi never set one, it
/// returns to the DPI read from the mouse before the shift.
#[derive(Default)]
struct HeldDpiShifts {
    by_press: HashMap<PressToken, String>,
    by_device: HashMap<String, HeldDpiShift>,
}

struct HeldDpiShift {
    presses: usize,
    key: String,
    route: DeviceRoute,
    before: DpiBeforeShift,
    /// The restore the shift's own write carries, from ending precision mode.
    own_restore: Option<Restore>,
}

impl HeldDpiShift {
    fn restore_to(&self, cycles: &DpiCycles) -> Option<Dpi> {
        cycles.current(&self.key).or_else(|| self.before.get())
    }
}

impl HeldDpiShifts {
    fn start(&mut self, executor: &ActionExecutor, press: &PressToken, device_key: Option<&str>) {
        if self.by_press.contains_key(press) {
            return;
        }
        let shift = executor.dpi_cycle.read().ok().and_then(|cycles| {
            let (key, low, route) = cycles.target_for_shift(device_key)?;
            let before = cycles.by_key[key].before_shift.clone();
            Some((key.to_owned(), low, route, cycles.current(key), before))
        });
        let Some((key, low, route, current, before)) = shift else {
            debug!("no DPI presets or target — DPI shift ignored");
            return;
        };
        let device = route.to_string();
        self.by_press.insert(press.clone(), device.clone());
        if let Some(held) = self.by_device.get_mut(&device) {
            held.presses += 1;
            return;
        }
        info!(%low, "DPI shift held");
        let write = executor.pointer_speeds.preset_write(&route, low);
        let own_restore = write.restore.clone();
        // The DPI the shift starts from stays its fallback: a rejected panel
        // write during the hold clears `current`.
        if let Some(dpi) = current {
            before.set(dpi);
        }
        executor.write_speed_reading(write, current.is_none().then(|| before.clone()));
        self.by_device.insert(
            device,
            HeldDpiShift {
                presses: 1,
                key,
                route,
                before,
                own_restore,
            },
        );
    }

    fn end(&mut self, executor: &ActionExecutor, press: &PressToken) {
        let Some(device) = self.by_press.remove(press) else {
            return;
        };
        let Some(held) = self.by_device.get_mut(&device) else {
            return;
        };
        held.presses -= 1;
        if held.presses > 0 {
            return;
        }
        let Some(held) = self.by_device.remove(&device) else {
            return;
        };
        let Some(ShiftRestore { ticket, restore }) = executor.pointer_speeds.shift_restore(
            &held.key,
            &held.route,
            held.own_restore.as_ref(),
        ) else {
            // Precision mode sets the DPI now, so the reading is stale.
            held.before.clear();
            debug!("precision mode holds the DPI — DPI shift restore skipped");
            return;
        };
        let cycles = Arc::clone(&executor.dpi_cycle);
        let speeds = Arc::clone(&executor.pointer_speeds);
        // Settling a restore ends the precision speed, so the LED goes dark.
        let led = restore.as_ref().map(|_| false);
        restore_dpi_in_background(
            executor.access.op(&held.route),
            ticket,
            held.before.clone(),
            led,
            move || held.restore_to(&*cycles.read().ok()?),
            move || {
                if let Some(restore) = restore {
                    speeds.restored(&restore);
                }
            },
        );
    }
}

struct ButtonEventHandler {
    executor: ActionExecutor,
    held: HeldShortcuts,
    held_dpi: HeldDpiShifts,
}

impl ButtonEventHandler {
    fn new(executor: ActionExecutor) -> Self {
        Self {
            executor,
            held: HeldShortcuts::default(),
            held_dpi: HeldDpiShifts::default(),
        }
    }

    fn handle(&mut self, event: ButtonRuntimeEvent) {
        match event {
            ButtonRuntimeEvent::Started(press) => {
                if let Some(action) = press.start_action() {
                    if press.captured_by_os_hook() {
                        log_bound_action(press.control(), action);
                    }
                    self.start_action(press.token(), action, press.device_key(), press.target());
                }
            }
            ButtonRuntimeEvent::Triggered { press, action } => {
                if press.captured_by_os_hook() {
                    log_bound_action(press.control(), &action);
                }
                self.start_action(press.token(), &action, press.device_key(), press.target());
            }
            ButtonRuntimeEvent::Ended { press, reason } => {
                self.held.end(press.token());
                self.held_dpi.end(&self.executor, press.token());
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
        target: ActionDispatchTarget,
    ) {
        if action.held_chord().is_some() && target.resolve(action).is_none() {
            return;
        }
        if matches!(action, Action::DpiShift) {
            self.held_dpi.start(&self.executor, press, device_key);
            return;
        }
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
        pointer_speeds: Arc<PointerSpeeds>,
        access: DeviceAccess,
        requests: tokio::sync::mpsc::UnboundedSender<ActionRequest>,
    ) -> io::Result<Self> {
        let executor = ActionExecutor {
            dpi_cycle,
            pointer_speeds,
            access,
            requests,
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
    /// Write `device_key`'s saved DPI again after its session entered host
    /// mode.
    pub(crate) fn host_mode_entered(&self, device_key: &str) {
        self.executor.host_mode_entered(device_key);
    }

    /// Route one action without blocking the input callback.
    pub fn dispatch(&self, action: &Action, device_key: Option<&str>) {
        self.executor.dispatch(action, device_key);
    }

    pub(crate) fn dispatch_pointer_action(
        &self,
        action: &Action,
        device_key: Option<&str>,
        pointer: Option<PointerPress>,
    ) {
        self.executor.dispatch_to(
            action,
            device_key,
            ActionDispatchTarget::for_pointer(pointer, openlogi_hook::frontmost_safari_pid),
        );
    }

    /// Queue one OS-hook down edge without blocking the callback. The returned
    /// token uniquely identifies this accepted press.
    pub(crate) fn try_hook_button_down(
        &self,
        button: ButtonId,
        binding: Option<&Binding>,
        target: ActionDispatchTarget,
    ) -> Option<PressToken> {
        self.buttons
            .try_hook_down_with_target(button, binding, target)
    }

    /// Queue one OS-hook up edge without blocking the callback.
    pub(crate) fn try_hook_button_up(&self, button: ButtonId) -> bool {
        self.buttons.try_hook_up(button)
    }

    /// Queue one function-key down edge without blocking the hook callback.
    pub(crate) fn try_hook_key_down(
        &self,
        keycode: u16,
        action: &Action,
        target: ActionDispatchTarget,
    ) -> bool {
        self.buttons
            .try_hook_key_down(keycode, action, target)
            .is_some()
    }

    /// Queue one function-key up edge without blocking the hook callback.
    pub(crate) fn try_hook_key_up(&self, keycode: u16) -> bool {
        self.buttons.try_hook_key_up(keycode)
    }

    /// Execute a semantic gesture action only if its exact press is still live.
    pub(crate) fn try_dispatch_while_pressed(&self, press: &PressToken, action: &Action) -> bool {
        self.buttons.try_trigger_while_pressed(press, action)
    }

    /// Record that the wheel moved while `press` was down, so a wheel-hold
    /// button's release does not also fire its click.
    pub(crate) fn try_wheel_while_pressed(&self, press: &PressToken) -> bool {
        self.buttons.try_wheel_while_pressed(press)
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
        pointer: Option<PointerPress>,
    ) -> Option<PressToken> {
        self.buttons.try_hidpp_down(
            session,
            button,
            binding,
            ActionDispatchTarget::for_pointer(pointer, openlogi_hook::frontmost_safari_pid),
        )
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
        pointer: Option<PointerPress>,
    ) {
        self.buttons.try_hidpp_pulse(
            session,
            button,
            binding,
            ActionDispatchTarget::for_pointer(pointer, openlogi_hook::frontmost_safari_pid),
        );
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

    /// End only pointer-scoped presses when the hovered window changes.
    /// Keyboard and explicitly focus-scoped holds keep their own lifecycles.
    /// Preserve presses already admitted against the newly published target.
    pub fn cancel_pointer_buttons_except(&self, current: openlogi_hook::PointerTarget) {
        self.buttons.cancel_pointer_except(current);
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

fn browser_nav_debounce_commit(reservation: BrowserNavDebounceReservation) {
    let mut last = BROWSER_NAV_LAST
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let slot = if reservation.forward {
        &mut last.1
    } else {
        &mut last.0
    };
    if *slot == Some(reservation.timestamp) {
        // AX may take longer than the debounce interval. Start that interval
        // at completion, before the button worker dequeues a duplicate path.
        *slot = Some(Instant::now());
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
    target: ActionDispatchTarget,
    ax_navigate: impl FnOnce(i32, bool) -> bool,
    keyboard: impl FnOnce(),
) -> bool {
    match target {
        ActionDispatchTarget::SafariProcess(pid) => {
            let navigated = ax_navigate(pid, matches!(action, Action::BrowserForward));
            if !navigated {
                info!(pid, action = %action.label(), "captured Safari navigation unavailable — keyboard fallback suppressed");
            }
            navigated
        }
        ActionDispatchTarget::Keyboard => {
            keyboard();
            true
        }
        ActionDispatchTarget::Pointer { .. } => {
            unreachable!("pointer targets resolve before navigation")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static BROWSER_NAV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn executor_with(pointer_speeds: Arc<PointerSpeeds>) -> ActionExecutor {
        let (_signal, device_io) = openlogi_hid::device_io_channel();
        let (requests, _requests_rx) = tokio::sync::mpsc::unbounded_channel();
        ActionExecutor {
            dpi_cycle: Arc::default(),
            pointer_speeds,
            access: DeviceAccess {
                channel: openlogi_hid::CaptureChannelSlot::default(),
                registry: openlogi_hid::ChannelRegistry::default(),
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io,
                transport_turns: crate::hardware::TransportTurns::default(),
            },
            requests,
        }
    }

    /// The MX Ergo's precision button keeps its default Cycle DPI Presets
    /// binding. The mouse has no DPI presets and scales its pointer with
    /// `0x2205`, so each press switches precision mode instead of being
    /// dropped (#1372). A mouse that is not scaled still drops the press.
    #[test]
    fn cycle_dpi_presets_toggles_precision_mode_on_a_scaled_mouse() {
        let pointer_speeds = Arc::new(PointerSpeeds::default());
        pointer_speeds.publish(
            None,
            [crate::pointer_speed::SpeedMouse {
                key: "ergo".to_string(),
                route: openlogi_core::hid::DeviceRoute::Unifying {
                    receiver_uid: "UNIFYING".to_string(),
                    slot: 1,
                },
                saved: crate::pointer_speed::SavedSpeed::Scaled(None),
            }],
        );
        let executor = executor_with(pointer_speeds.clone());

        executor.dispatch(&Action::CycleDpiPresets, Some("ergo"));
        assert!(pointer_speeds.precision("ergo"));
        executor.dispatch(&Action::CycleDpiPresets, Some("ergo"));
        assert!(!pointer_speeds.precision("ergo"));

        executor.dispatch(&Action::CycleDpiPresets, Some("master"));
        assert!(!pointer_speeds.precision("master"));
    }

    /// Presets saved for a scaled mouse, by an older panel or by hand, do not
    /// take the press: the mouse has no DPI to write them to.
    #[test]
    fn cycle_dpi_presets_prefers_precision_mode_over_saved_presets() {
        let route = openlogi_core::hid::DeviceRoute::Unifying {
            receiver_uid: "UNIFYING".to_string(),
            slot: 1,
        };
        let pointer_speeds = Arc::new(PointerSpeeds::default());
        pointer_speeds.publish(
            None,
            [crate::pointer_speed::SpeedMouse {
                key: "ergo".to_string(),
                route: route.clone(),
                saved: crate::pointer_speed::SavedSpeed::Scaled(None),
            }],
        );
        let mut executor = executor_with(pointer_speeds.clone());
        let mut cycles = DpiCycles::default();
        cycles.by_key.insert(
            "ergo".to_string(),
            DpiCycleState {
                presets: vec![
                    openlogi_core::hid::Dpi::new(800),
                    openlogi_core::hid::Dpi::new(1600),
                ],
                index: 0,
                target: Some(route),
                capabilities: None,
                ..DpiCycleState::default()
            },
        );
        executor.dpi_cycle = Arc::new(RwLock::new(cycles));

        executor.dispatch(&Action::CycleDpiPresets, Some("ergo"));

        assert!(pointer_speeds.precision("ergo"));
        let cycles = executor.dpi_cycle.read().expect("dpi_cycle lock");
        assert_eq!(cycles.by_key["ergo"].index, 0, "the presets did not cycle");
    }

    /// A DPI mouse has DPI to cycle, so its DPI button keeps cycling the
    /// presets (#1931). Its precision button sets the lowest DPI, and a
    /// preset press ends precision mode: the mouse leaves the lowest DPI.
    #[test]
    fn a_dpi_mouse_cycles_its_presets_and_a_preset_ends_precision_mode() {
        let route = openlogi_core::hid::DeviceRoute::Unifying {
            receiver_uid: "UNIFYING".to_string(),
            slot: 2,
        };
        let pointer_speeds = Arc::new(PointerSpeeds::default());
        pointer_speeds.publish(
            None,
            [crate::pointer_speed::SpeedMouse {
                key: "master".to_string(),
                route: route.clone(),
                saved: crate::pointer_speed::SavedSpeed::Dpi(None),
            }],
        );
        let mut executor = executor_with(pointer_speeds.clone());
        let mut cycles = DpiCycles::default();
        cycles.by_key.insert(
            "master".to_string(),
            DpiCycleState {
                presets: vec![
                    openlogi_core::hid::Dpi::new(800),
                    openlogi_core::hid::Dpi::new(1600),
                ],
                index: 0,
                target: Some(route),
                capabilities: None,
                ..DpiCycleState::default()
            },
        );
        executor.dpi_cycle = Arc::new(RwLock::new(cycles));

        executor.dispatch(&Action::TogglePrecisionMode, Some("master"));
        assert!(pointer_speeds.precision("master"));

        executor.dispatch(&Action::CycleDpiPresets, Some("master"));
        assert!(!pointer_speeds.precision("master"));
        // No mouse answers in this test, so the preset write never lands
        // and the restore stays owed.
        assert!(pointer_speeds.restore_pending("master"));
        let cycles = executor.dpi_cycle.read().expect("dpi_cycle lock");
        assert_eq!(cycles.by_key["master"].index, 1, "the presets cycled");
    }

    #[test]
    fn ring_trigger_reaches_the_ring_over_an_unavailable_pointer_target() {
        // The second press that toggles the ring closed lands on the ring's
        // own overlay window, which the pointer hit test reports as
        // unavailable.
        let (_signal, device_io) = openlogi_hid::device_io_channel();
        let (requests, mut ring_rx) = tokio::sync::mpsc::unbounded_channel();
        let executor = ActionExecutor {
            dpi_cycle: Arc::default(),
            pointer_speeds: Arc::default(),
            access: DeviceAccess {
                channel: openlogi_hid::CaptureChannelSlot::default(),
                registry: openlogi_hid::ChannelRegistry::default(),
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io,
                transport_turns: crate::hardware::TransportTurns::default(),
            },
            requests,
        };

        executor.dispatch_to(
            &Action::ShowActionsRing,
            Some("mouse"),
            ActionDispatchTarget::for_pointer(
                Some(PointerPress::distinct(
                    openlogi_hook::PointerTarget::Unavailable,
                )),
                || None,
            ),
        );

        assert_eq!(
            ring_rx.try_recv(),
            Ok(ActionRequest::ShowActionsRing {
                device_key: Some("mouse".to_owned())
            })
        );
    }
    /// Page actions go to the agent, keyed by the pressing keypad, and — like
    /// the ring — no pointer target gates them: the press acts on the keypad.
    #[test]
    fn page_actions_reach_the_agent_as_page_turns() {
        let (_signal, device_io) = openlogi_hid::device_io_channel();
        let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
        let executor = ActionExecutor {
            dpi_cycle: Arc::default(),
            pointer_speeds: Arc::default(),
            access: DeviceAccess {
                channel: openlogi_hid::CaptureChannelSlot::default(),
                registry: openlogi_hid::ChannelRegistry::default(),
                receiver_access: crate::receiver_access::ReceiverAccess::default(),
                device_io,
                transport_turns: crate::hardware::TransportTurns::default(),
            },
            requests,
        };
        let unavailable = || {
            ActionDispatchTarget::for_pointer(
                Some(PointerPress::distinct(
                    openlogi_hook::PointerTarget::Unavailable,
                )),
                || None,
            )
        };

        executor.dispatch_to(&Action::NextKeypadPage, Some("keypad"), unavailable());
        executor.dispatch_to(&Action::PreviousKeypadPage, Some("keypad"), unavailable());

        for turn in [PageTurn::Next, PageTurn::Previous] {
            assert_eq!(
                received.try_recv(),
                Ok(ActionRequest::TurnKeypadPage {
                    device_key: Some("keypad".to_owned()),
                    turn,
                })
            );
        }
    }

    #[test]
    fn instantaneous_actions_do_not_enter_held_state() {
        let press = PressToken::hook_for_test(1, ButtonId::Back);
        let mut held = HeldShortcuts::default();

        assert!(!held.start(&press, &Action::Copy));
        held.end(&press);
    }

    /// An executor for one mouse with presets and no reachable device.
    fn dpi_shift_executor(current: Option<Dpi>) -> ActionExecutor {
        let mut cycles = DpiCycles::default();
        cycles.by_key.insert(
            "mouse".into(),
            DpiCycleState {
                presets: vec![Dpi::new(400), Dpi::new(1600)],
                target: Some(DeviceRoute::Bolt {
                    receiver_uid: "AA00".into(),
                    slot: 1,
                }),
                current,
                ..DpiCycleState::default()
            },
        );
        let mut executor = executor_with(Arc::default());
        executor.dpi_cycle = Arc::new(RwLock::new(cycles));
        executor
    }

    /// A preset step reads the mouse first only while OpenLogi never set its
    /// DPI; once it did, the index is where the mouse is.
    #[test]
    fn a_dpi_step_reads_the_mouse_only_while_its_dpi_is_unknown() {
        let unknown = dpi_shift_executor(None);
        assert!(unknown.step_dpi_from_reading(Some("mouse"), &Action::NextDpiPreset));
        assert!(unknown.step_dpi_from_reading(Some("mouse"), &Action::PreviousDpiPreset));
        assert!(!unknown.step_dpi_from_reading(Some("missing"), &Action::NextDpiPreset));

        let known = dpi_shift_executor(Some(Dpi::new(1600)));
        assert!(!known.step_dpi_from_reading(Some("mouse"), &Action::NextDpiPreset));
    }

    /// A preset step caches its DPI when the write is requested. When the
    /// mouse rejects it, the next DPI shift must not return to that DPI: the
    /// rejection forgets it, so the shift reads the mouse and returns there.
    #[test]
    fn a_rejected_preset_step_leaves_the_dpi_shift_returning_to_the_old_dpi() {
        let executor = dpi_shift_executor(Some(Dpi::new(400)));
        let route = DeviceRoute::Bolt {
            receiver_uid: "AA00".into(),
            slot: 1,
        };
        let stepped = executor.step_dpi(Some("mouse"), &Action::NextDpiPreset);
        assert_eq!(stepped.map(|(dpi, _)| dpi), Some(Dpi::new(1600)));
        executor.preset_rejected(route)(Dpi::new(1600));
        assert_eq!(executor.dpi_cycle.read().unwrap().current("mouse"), None);

        let press = PressToken::hook_for_test(1, ButtonId::G6);
        let mut held = HeldDpiShifts::default();
        held.start(&executor, &press, Some("mouse"));
        let shift = held.by_device.values().next().unwrap();
        // What the shift's own write reads from the mouse, which kept 400.
        shift.before.set(Dpi::new(400));
        let cycles = executor.dpi_cycle.read().unwrap();
        assert_eq!(shift.restore_to(&cycles), Some(Dpi::new(400)));
    }

    /// A rejection that arrives after a newer preset pick leaves that pick.
    #[test]
    fn a_rejected_preset_step_keeps_a_newer_pick() {
        let executor = dpi_shift_executor(Some(Dpi::new(400)));
        let route = DeviceRoute::Bolt {
            receiver_uid: "AA00".into(),
            slot: 1,
        };
        executor.step_dpi(Some("mouse"), &Action::NextDpiPreset);
        executor.step_dpi(Some("mouse"), &Action::PreviousDpiPreset);
        executor.preset_rejected(route)(Dpi::new(1600));
        assert_eq!(
            executor.dpi_cycle.read().unwrap().current("mouse"),
            Some(Dpi::new(400))
        );
    }

    #[test]
    fn dpi_shift_ends_only_with_the_last_held_press() {
        let executor = dpi_shift_executor(Some(Dpi::new(1200)));
        let (first, second) = (
            PressToken::hook_for_test(1, ButtonId::G6),
            PressToken::hook_for_test(2, ButtonId::G7),
        );
        let mut held = HeldDpiShifts::default();
        held.start(&executor, &first, Some("mouse"));
        held.start(&executor, &second, Some("mouse"));
        held.end(&executor, &first);
        assert_eq!(held.by_device.values().next().map(|h| h.presses), Some(1));
        held.end(&executor, &second);
        assert!(held.by_device.is_empty() && held.by_press.is_empty());
    }

    /// A press right after a release can supersede the pending restore while
    /// the sensor is still at the shifted DPI, so it must not read again.
    #[test]
    fn dpi_shift_keeps_its_reading_until_a_restore_is_written() {
        let executor = dpi_shift_executor(None);
        let (first, second) = (
            PressToken::hook_for_test(1, ButtonId::G6),
            PressToken::hook_for_test(2, ButtonId::G6),
        );
        let mut held = HeldDpiShifts::default();
        held.start(&executor, &first, Some("mouse"));
        let before = held.by_device.values().next().unwrap().before.clone();
        before.set(Dpi::new(1200));
        held.end(&executor, &first);

        held.start(&executor, &second, Some("mouse"));
        let again = &held.by_device.values().next().unwrap().before;
        assert_eq!(again.get(), Some(Dpi::new(1200)));
        before.clear();
        assert_eq!(again.get(), None, "a written restore forgets the reading");
    }

    /// A failed restore keeps the reading, but a reconnect puts the mouse at
    /// its onboard DPI, so the next hold must read it again.
    #[test]
    fn a_dpi_shift_after_a_reconnect_reads_the_mouse_again() {
        let executor = dpi_shift_executor(None);
        let press = PressToken::hook_for_test(1, ButtonId::G6);
        executor.dpi_cycle.read().unwrap().by_key["mouse"]
            .before_shift
            .set(Dpi::new(1200));
        executor.dpi_cycle.write().unwrap().reconnected("mouse");

        let mut held = HeldDpiShifts::default();
        held.start(&executor, &press, Some("mouse"));
        assert_eq!(held.by_device.values().next().unwrap().before.get(), None);
    }

    /// A panel write the mouse rejects during a hold clears `current`, so
    /// the release falls back to the DPI the shift started from.
    #[test]
    fn a_rejected_panel_dpi_during_a_shift_still_returns_to_the_start() {
        let executor = dpi_shift_executor(Some(Dpi::new(1200)));
        let press = PressToken::hook_for_test(1, ButtonId::G6);
        let mut held = HeldDpiShifts::default();
        held.start(&executor, &press, Some("mouse"));

        let route = DeviceRoute::Bolt {
            receiver_uid: "AA00".into(),
            slot: 1,
        };
        let mut cycles = executor.dpi_cycle.write().unwrap();
        cycles.reapplied_at(&route, Dpi::new(1600));
        cycles.rejected_at(&route, Dpi::new(1600));
        let shift = held.by_device.values().next().unwrap();
        assert_eq!(shift.restore_to(&cycles), Some(Dpi::new(1200)));
    }

    #[test]
    fn dpi_shift_returns_to_the_dpi_last_set_or_else_the_one_it_read() {
        let mut cycles = DpiCycles::default();
        cycles.by_key.insert(
            "mouse".into(),
            DpiCycleState {
                presets: vec![Dpi::new(400), Dpi::new(1600)],
                ..DpiCycleState::default()
            },
        );
        let held = HeldDpiShift {
            presses: 1,
            key: "mouse".into(),
            route: DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 1,
            },
            before: DpiBeforeShift::default(),
            own_restore: None,
        };
        // Never set and not read yet: a preset is not the DPI to return to.
        assert_eq!(held.restore_to(&cycles), None);
        held.before.set(Dpi::new(1200));
        assert_eq!(held.restore_to(&cycles), Some(Dpi::new(1200)));
        // A preset picked during the hold stays after release.
        cycles.state_for(Some("mouse")).unwrap().step_up();
        assert_eq!(held.restore_to(&cycles), Some(Dpi::new(1600)));
    }

    /// Precision mode turned on during the hold sets the DPI, and its write
    /// may still be waiting, so the release writes nothing after it and
    /// forgets its reading.
    #[test]
    fn dpi_shift_release_leaves_precision_mode_alone() {
        let speeds = Arc::new(PointerSpeeds::default());
        speeds.publish(
            None,
            [crate::pointer_speed::SpeedMouse {
                key: "mouse".to_string(),
                route: DeviceRoute::Bolt {
                    receiver_uid: "AA00".into(),
                    slot: 1,
                },
                saved: crate::pointer_speed::SavedSpeed::Dpi(Some(Dpi::new(1600))),
            }],
        );
        let mut executor = dpi_shift_executor(None);
        executor.pointer_speeds = Arc::clone(&speeds);
        let press = PressToken::hook_for_test(1, ButtonId::G7);
        let mut held = HeldDpiShifts::default();
        held.start(&executor, &press, Some("mouse"));
        let before = held.by_device.values().next().unwrap().before.clone();
        before.set(Dpi::new(1200));
        let (on, _) = speeds
            .toggle_precision(Some("mouse"), PrecisionPress::Assigned)
            .expect("a DPI mouse toggles precision mode");
        assert!(on);

        held.end(&executor, &press);
        assert!(held.by_device.is_empty());
        assert_eq!(before.get(), None, "the reading goes with the restore");
    }

    /// A quick press while precision mode is on: the shift's own low write
    /// ended the mode and still waits when the press ends. The release must
    /// restore anyway, and owe nothing once it has.
    #[test]
    fn a_quick_dpi_shift_in_precision_mode_still_restores() {
        let speeds = Arc::new(PointerSpeeds::default());
        speeds.publish(
            None,
            [crate::pointer_speed::SpeedMouse {
                key: "mouse".to_string(),
                route: DeviceRoute::Bolt {
                    receiver_uid: "AA00".into(),
                    slot: 1,
                },
                saved: crate::pointer_speed::SavedSpeed::Dpi(Some(Dpi::new(1600))),
            }],
        );
        speeds
            .toggle_precision(Some("mouse"), PrecisionPress::Assigned)
            .expect("precision on");
        let mut executor = dpi_shift_executor(None);
        executor.pointer_speeds = Arc::clone(&speeds);
        let press = PressToken::hook_for_test(1, ButtonId::G7);
        let mut held = HeldDpiShifts::default();
        held.start(&executor, &press, Some("mouse"));
        let before = held.by_device.values().next().unwrap().before.clone();
        before.set(Dpi::new(1200));

        held.end(&executor, &press);
        assert!(held.by_device.is_empty());
        assert_eq!(
            before.get(),
            Some(Dpi::new(1200)),
            "the release restores, so it keeps the reading until it lands"
        );
    }

    /// After a session enters host mode the saved DPI is written again, and
    /// a DPI shift returns to it rather than to a preset picked before.
    #[test]
    fn entering_host_mode_writes_the_saved_dpi_again() {
        let speeds = Arc::new(PointerSpeeds::default());
        speeds.publish(
            None,
            [crate::pointer_speed::SpeedMouse {
                key: "mouse".to_string(),
                route: DeviceRoute::Bolt {
                    receiver_uid: "AA00".into(),
                    slot: 1,
                },
                saved: crate::pointer_speed::SavedSpeed::Dpi(Some(Dpi::new(1600))),
            }],
        );
        let mut executor = dpi_shift_executor(Some(Dpi::new(400)));
        executor.pointer_speeds = speeds;

        executor.host_mode_entered("mouse");
        let cycles = executor.dpi_cycle.read().expect("dpi_cycle lock");
        assert_eq!(cycles.current("mouse"), Some(Dpi::new(1600)));
    }

    #[test]
    fn captured_safari_navigation_keeps_press_time_pid_and_never_falls_back() {
        for (action, forward) in [(Action::BrowserBack, false), (Action::BrowserForward, true)] {
            for navigated in [false, true] {
                let mut pid_and_direction = None;
                let mut keyboard = false;
                assert_eq!(
                    dispatch_browser_navigation(
                        &action,
                        ActionDispatchTarget::SafariProcess(417),
                        |pid, direction| {
                            pid_and_direction = Some((pid, direction));
                            navigated
                        },
                        || keyboard = true,
                    ),
                    navigated
                );
                assert_eq!(pid_and_direction, Some((417, forward)));
                assert!(!keyboard);
            }
        }
    }

    #[test]
    fn keyboard_navigation_never_reinterprets_a_later_safari_focus() {
        let mut ax_called = false;
        let mut keyboard = false;

        assert!(dispatch_browser_navigation(
            &Action::BrowserForward,
            ActionDispatchTarget::Keyboard,
            |_, _| {
                ax_called = true;
                true
            },
            || keyboard = true,
        ));

        assert!(!ax_called);
        assert!(keyboard);
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
    fn slow_successful_navigation_still_debounces_the_queued_duplicate() {
        let _guard = BROWSER_NAV_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let started = Instant::now()
            .checked_sub(BROWSER_NAV_DEBOUNCE * 2)
            .expect("the simulated AX duration fits before the current instant");
        *BROWSER_NAV_LAST
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = (None, Some(started));

        browser_nav_debounce_commit(BrowserNavDebounceReservation {
            forward: true,
            timestamp: started,
        });

        assert!(browser_nav_debounce_begin(&Action::BrowserForward).is_none());
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

        assert!(!dispatch_browser_navigation(
            &Action::BrowserBack,
            ActionDispatchTarget::SafariProcess(417),
            |_, _| false,
            || panic!("stale Safari targets must not fall back"),
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
