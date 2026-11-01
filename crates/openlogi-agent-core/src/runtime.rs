//! Shared action runtime for every background input source.
//!
//! [`ActionRuntime`] uniquely owns lifecycle resources, while cloneable
//! [`ActionDispatcher`] values let OS-hook and HID++ producers submit work
//! without owning worker shutdown. Source-specific hook interpretation lives
//! in [`hook`]; the button state machine remains an internal implementation.

mod button;
mod held;
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
use self::held::{ButtonOutput, HeldActions, NativeButtonOutput};
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

/// How long a started action's output may stay held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldScope {
    /// The physical press: a mouse button or a chord stays down until release.
    Press,
    /// A gesture commit or release-time trigger: only a chord stays down.
    Trigger,
}

struct ButtonEventHandler<B: ButtonOutput = NativeButtonOutput> {
    executor: ActionExecutor,
    held: HeldActions<B>,
    held_dpi: HeldDpiShifts,
}

impl ButtonEventHandler {
    fn new(executor: ActionExecutor) -> Self {
        Self {
            executor,
            held: HeldActions::default(),
            held_dpi: HeldDpiShifts::default(),
        }
    }
}

impl<B: ButtonOutput> ButtonEventHandler<B> {
    fn handle(&mut self, event: ButtonRuntimeEvent) {
        match event {
            ButtonRuntimeEvent::Started(press) => {
                if let Some(action) = press.start_action() {
                    if press.captured_by_os_hook() {
                        log_bound_action(press.control(), action);
                    }
                    self.start_action(
                        press.token(),
                        action,
                        press.device_key(),
                        press.target(),
                        HoldScope::Press,
                    );
                }
            }
            ButtonRuntimeEvent::Triggered { press, action } => {
                if press.captured_by_os_hook() {
                    log_bound_action(press.control(), &action);
                }
                self.start_action(
                    press.token(),
                    &action,
                    press.device_key(),
                    press.target(),
                    HoldScope::Trigger,
                );
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
        scope: HoldScope,
    ) {
        if matches!(action, Action::DpiShift) {
            self.held_dpi.start(&self.executor, press, device_key);
            return;
        }
        let held = match scope {
            HoldScope::Press => self.held.start(press, action, target),
            HoldScope::Trigger => self.held.start_shortcut(press, action, target),
        };
        if !held {
            self.held
                .dispatch(&self.executor, action, device_key, target);
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
mod tests;
