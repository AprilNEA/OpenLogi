//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    Action, ButtonId, GestureDirection, SwipeAccumulator, default_binding,
};
use openlogi_core::config::{KeyModifiers, KeyTrigger};
use openlogi_hid::{CaptureChannel, ChannelRegistry};
use openlogi_hook::{
    EventDevice, EventDisposition, Hook, HookEvent, MouseEvent, source_is_remappable,
};
use tracing::{info, warn};

use crate::button_runtime::{
    ButtonInputHandle, ButtonRuntimeEvent, ButtonRuntimeOwner, EndReason, HidppSessionId,
    PressToken,
};
use crate::event_monitor::SharedEventMonitor;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};
use crate::receiver_access::ReceiverAccess;
use crate::{DpiCycleState, DpiCycles};

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
        dispatch_action(
            action,
            &self.dpi_cycle,
            device_key,
            &self.capture,
            Some(&self.registry),
            &self.receiver_access,
        );
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
        let button_executor = executor.clone();
        let buttons = ButtonRuntimeOwner::spawn(move |event| match event {
            ButtonRuntimeEvent::Started(press) => {
                if let Some(action) = press.action() {
                    button_executor.dispatch(action, press.device_key());
                }
            }
            ButtonRuntimeEvent::Triggered { press, action } => {
                button_executor.dispatch(&action, press.device_key());
            }
            ButtonRuntimeEvent::Ended {
                press,
                reason: EndReason::Canceled(reason),
            } => {
                info!(button = %press.button(), ?reason, "button lifecycle canceled");
            }
            ButtonRuntimeEvent::Ended {
                reason: EndReason::Released,
                ..
            } => {}
        })?;
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
    /// This is the terminal edge for [`MouseEvent::CaptureInterrupted`].
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

/// The two button maps the OS-hook callback reads, kept behind ONE lock so a
/// config rebuild publishes both atomically — a press during an owner switch can
/// never see the new single-action bindings against the old gesture map (or vice
/// versa), and the common case reads one lock instead of two.
#[derive(Default)]
pub struct HookMaps {
    /// Per-button single action — the single-action dispatch path.
    pub bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction maps for the OS-hook gesture buttons (Middle/Back/Forward in
    /// gesture mode), so a hold+swipe resolves to a bound action. The dedicated
    /// HID++ gesture button (0x00c3) uses the gesture watcher's separate map
    /// instead — it never reaches the OS hook.
    pub gestures: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
}

/// Shared, atomically-published [`HookMaps`], threaded between the config owner
/// (orchestrator), the OS-hook callback, and the gesture watcher.
pub type SharedHookMaps = Arc<RwLock<HookMaps>>;

/// Shared keyboard trigger→action map for the function-key remapper. Unlike
/// mouse bindings these are not per-app-profile (M1 scope — per the spec's
/// non-goals), so a single map suffices. Keyed by the config `KeyTrigger`
/// (keycode + modifiers).
pub type SharedKeyboardBindings = Arc<RwLock<BTreeMap<KeyTrigger, Action>>>;

/// Convert the hook-layer modifier state into the config-layer type (the two
/// live in different crates — core is leaf-level and duplicates the four
/// bools). Drop-in identity once the field names align.
fn convert_modifiers(m: openlogi_hook::KeyModifiers) -> KeyModifiers {
    KeyModifiers {
        shift: m.shift,
        control: m.control,
        option: m.option,
        command: m.command,
    }
}

/// Tracks which OS-hook button (Middle/Back/Forward) is mid-hold and defers the
/// swipe detection itself to a shared [`SwipeAccumulator`], which commits a swipe
/// *mid-motion* like the HID++ gesture-button path in `openlogi-hid`. This wrapper
/// adds only the button identity the accumulator doesn't track; a press that
/// never commits a direction is a plain click, fired on release.
/// A gesture hold this old is presumed stale — real hold+swipe interactions
/// finish in well under a second, and only a lost button-up (with no OS
/// interrupt to trigger [`HoldState::cancel`]) leaves one lingering.
const STALE_HOLD: Duration = Duration::from_secs(10);

#[derive(Default)]
struct HoldState {
    current: Option<GestureHold>,
    swipe: SwipeAccumulator,
}

struct GestureHold {
    button: ButtonId,
    started_at: Instant,
    press: PressToken,
}

enum HoldAdmission {
    Begin,
    Replace(PressToken),
    Refuse,
}

impl HoldState {
    /// Prepare a hold for `button`. With several gesture buttons the first live
    /// hold wins, so a second button cannot hijack accumulated motion. The
    /// caller obtains a fresh [`PressToken`] only after this admission step.
    ///
    /// Two presses recover a hold whose button-up was lost (nothing else ever
    /// clears it when the OS drops a release without an interrupt): a re-press
    /// of the held button itself — a button cannot be pressed while down, so
    /// this is proof the release was lost — and any press once the hold has
    /// aged past [`STALE_HOLD`], without which every other gesture button
    /// would stay refused indefinitely.
    fn prepare_begin(&mut self, button: ButtonId) -> HoldAdmission {
        let Some(held) = self.current.take() else {
            return HoldAdmission::Begin;
        };
        if held.button != button && held.started_at.elapsed() < STALE_HOLD {
            self.current = Some(held);
            return HoldAdmission::Refuse;
        }

        self.swipe.end();
        if held.button == button {
            HoldAdmission::Begin
        } else {
            HoldAdmission::Replace(held.press)
        }
    }

    /// Store the token returned by the accepted lifecycle `Down`.
    fn begin(&mut self, button: ButtonId, press: PressToken) {
        self.current = Some(GestureHold {
            button,
            started_at: Instant::now(),
            press,
        });
        self.swipe.begin();
    }

    /// Feed a pointer-move delta into the active hold, tagging a committed swipe
    /// with its exact press token and held button. Returns one commit per hold,
    /// or `None` while still too short, already fired, or not holding.
    fn accumulate(&mut self, dx: i32, dy: i32) -> Option<(PressToken, ButtonId, GestureDirection)> {
        let held = self.current.as_ref()?;
        self.swipe
            .accumulate(dx, dy)
            .map(|dir| (held.press.clone(), held.button, dir))
    }

    /// End the hold for `button`, returning its exact token and whether it was a
    /// click. A swipe returns `false`; a stray release returns `None`.
    fn end(&mut self, button: ButtonId) -> Option<(PressToken, bool)> {
        let held = self.current.take_if(|held| held.button == button)?;
        let was_click = self.swipe.end();
        Some((held.press, was_click))
    }

    /// Cancel any in-progress hold without firing anything — used when the OS
    /// interrupts capture. A dropped button-up would otherwise leave a stale hold
    /// that the next stray pointer move turns into a phantom swipe.
    fn cancel(&mut self) {
        self.current = None;
        self.swipe.end();
    }

    /// Age the current hold past the staleness horizon, so tests can exercise
    /// the lost-button-up recovery without sleeping.
    #[cfg(test)]
    fn backdate_for_test(&mut self) {
        if let Some(held) = &mut self.current
            && let Some(aged) = Instant::now().checked_sub(STALE_HOLD)
        {
            held.started_at = aged;
        }
    }
}

thread_local! {
    /// In-progress gesture hold, one instance per hook-callback thread: the
    /// single macOS tap thread, or — on Linux — one thread per device, so two
    /// mice never share a hold (a press on one can't hijack the other's swipe).
    /// Thread-local rather than a shared `Mutex` keeps the hot path lock-free and
    /// free of cross-thread contention on the freeze-sensitive callback.
    static HOLD: RefCell<HoldState> = RefCell::new(HoldState::default());
    /// Buttons whose physical press was delivered because the action queue
    /// rejected the remap. Their matching release must also pass through so
    /// apps never see a stuck auxiliary button (down without up).
    static FAIL_OPEN_PRESSES: RefCell<HashSet<ButtonId>> = RefCell::new(HashSet::new());
}

/// Whether a button event's physical source may be remapped/suppressed.
///
/// macOS attributes every CGEvent to an IOKit sender and fails closed: only
/// known Logitech non-trackpad devices are remappable, so the built-in
/// trackpad can never be swallowed. Linux/Windows often lack attribution
/// (`device: None`); those platforms already restrict which devices the hook
/// attaches to, so unknown sources stay remappable.
fn button_source_may_remap(device: Option<&EventDevice>) -> bool {
    match device {
        Some(d) => source_is_remappable(Some(d)),
        None => {
            // Attribution missing: safe on Linux/Windows (device selection is
            // upstream of the callback). On macOS fail closed — an unattributed
            // event is more likely a trackpad/system source than a Logi mouse.
            !cfg!(target_os = "macos")
        }
    }
}

/// Off-thread worker for bound actions so the tap callback never injects input.
fn spawn_action_worker(dispatcher: ActionDispatcher) -> mpsc::SyncSender<Action> {
    let (tx, rx) = mpsc::sync_channel::<Action>(64);
    let _ = thread::Builder::new()
        .name("openlogi-action".into())
        .spawn(move || {
            while let Ok(action) = rx.recv() {
                dispatcher.dispatch(&action, None);
            }
        });
    tx
}

/// Queue a bound action without blocking the tap callback. Returns `false` if
/// the queue is full (caller should fail open and pass the physical event).
fn try_queue_action(tx: &mpsc::SyncSender<Action>, action: Action) -> bool {
    if tx.try_send(action).is_err() {
        warn!("action queue full — dropping bound action to keep the input hook live");
        false
    } else {
        true
    }
}

/// Remap path for Middle/Back/Forward. Must stay lock-light and non-blocking.
fn handle_button(
    id: ButtonId,
    pressed: bool,
    device: Option<&EventDevice>,
    hooks: &SharedHookMaps,
    dispatcher: &ActionDispatcher,
) -> EventDisposition {
    // Primary L/R always pass through (suppressing them would brick the mouse).
    if !id.is_os_hook_button() || !button_source_may_remap(device) {
        return EventDisposition::PassThrough;
    }

    // `try_read` only: a blocking read on the tap thread freezes every pointer
    // event while a config rebuild holds the write lock. Fail open if unavailable.
    if pressed {
        let is_gesture = hooks.try_read().is_ok_and(|m| m.gestures.contains_key(&id));
        // A refused begin — a second gesture button pressed mid-hold — falls
        // through to the single-action path: the first hold wins and this press
        // still means its plain click.
        let admission = is_gesture.then(|| HOLD.with_borrow_mut(|h| h.prepare_begin(id)));
        if let Some(HoldAdmission::Begin | HoldAdmission::Replace(_)) = &admission {
            if let Some(HoldAdmission::Replace(stale)) = &admission {
                dispatcher.cancel_stale_hook_press(stale);
            }
            if let Some(press) = dispatcher.try_hook_button_down(id, None) {
                HOLD.with_borrow_mut(|h| h.begin(id, press));
                return EventDisposition::Suppress;
            }
            return FAIL_OPEN_PRESSES.with_borrow_mut(|s| remapped_press_disposition(id, false, s));
        }
    } else {
        // Drop the HOLD borrow before any queueing (re-entrancy freeze hazard).
        let ended = HOLD.with_borrow_mut(|h| h.end(id));
        if let Some((press, was_click)) = ended {
            if was_click {
                let action = hooks
                    .try_read()
                    .ok()
                    .map(|m| resolve_gesture_click(&m.gestures, id));
                if let Some(action) = action {
                    info!(button = %id, action = %action.label(), "gesture click → executing bound action");
                    dispatcher.try_dispatch_while_pressed(&press, &action);
                }
            }
            dispatcher.try_hook_button_up(id);
            return EventDisposition::Suppress;
        }
    }

    let action = hooks
        .try_read()
        .ok()
        .and_then(|m| m.bindings.get(&id).cloned());
    let Some(action) = action else {
        return EventDisposition::PassThrough;
    };
    if is_native_click(id, &action) {
        return EventDisposition::PassThrough;
    }
    if pressed {
        info!(button = %id, action = %action.label(), "button → executing bound action");
        let queued = dispatcher.try_hook_button_down(id, Some(&action)).is_some();
        return FAIL_OPEN_PRESSES.with_borrow_mut(|s| remapped_press_disposition(id, queued, s));
    }
    dispatcher.try_hook_button_up(id);
    FAIL_OPEN_PRESSES.with_borrow_mut(|s| remapped_release_disposition(id, s))
}

/// Press of a remapped single-action button: suppress when the action was
/// queued, otherwise pass through and mark `id` so the release pairs.
fn remapped_press_disposition(
    id: ButtonId,
    queued: bool,
    fail_open: &mut HashSet<ButtonId>,
) -> EventDisposition {
    if queued {
        fail_open.remove(&id);
        EventDisposition::Suppress
    } else {
        fail_open.insert(id);
        EventDisposition::PassThrough
    }
}

/// Release of a remapped single-action button: pass through only when the
/// matching press was fail-opened (queue rejection), else suppress.
fn remapped_release_disposition(
    id: ButtonId,
    fail_open: &mut HashSet<ButtonId>,
) -> EventDisposition {
    if fail_open.remove(&id) {
        EventDisposition::PassThrough
    } else {
        EventDisposition::Suppress
    }
}

/// Feed an in-progress gesture hold; always pass motion through so the cursor moves.
fn handle_moved(
    delta_x: i32,
    delta_y: i32,
    hooks: &SharedHookMaps,
    dispatcher: &ActionDispatcher,
) -> EventDisposition {
    let commit = HOLD.with_borrow_mut(|h| h.accumulate(delta_x, delta_y));
    if let Some((press, button, dir)) = commit {
        let action = hooks.try_read().ok().map(|m| {
            m.gestures
                .get(&button)
                .and_then(|dirs| dirs.get(&dir).cloned())
                .unwrap_or_else(|| resolve_gesture_click(&m.gestures, button))
        });
        if let Some(action) = action {
            info!(button = %button, ?dir, action = %action.label(), "gesture swipe → executing bound action");
            dispatcher.try_dispatch_while_pressed(&press, &action);
        }
    }
    EventDisposition::PassThrough
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    hooks: SharedHookMaps,
    keyboard_bindings: SharedKeyboardBindings,
    dispatcher: ActionDispatcher,
    monitor: SharedEventMonitor,
) -> Option<Hook> {
    if !Hook::has_accessibility() {
        warn!(
            "Accessibility not granted — events will not be captured. \
             Open System Settings → Privacy & Security → Accessibility."
        );
        return None;
    }

    // Actions never run on the tap callback thread (HID CGEventTap freeze hazard).
    let action_tx = spawn_action_worker(dispatcher.clone());

    // The per-hold pointer accumulator lives in the thread-local `HOLD`; the
    // callback must never block — see the freeze-hazard note in `macos.rs`.
    let result = Hook::start(move |event| match event {
        HookEvent::Mouse(event) => {
            monitor.record(&event);
            match event {
                MouseEvent::Button {
                    id,
                    pressed,
                    device,
                } => handle_button(id, pressed, device.as_ref(), &hooks, &dispatcher),
                MouseEvent::Moved { delta_x, delta_y } => {
                    handle_moved(delta_x, delta_y, &hooks, &dispatcher)
                }
                MouseEvent::CaptureInterrupted => {
                    HOLD.with_borrow_mut(HoldState::cancel);
                    dispatcher.cancel_hook_thread_buttons();
                    EventDisposition::PassThrough
                }
                MouseEvent::Scroll {
                    delta_x, delta_y, ..
                } => {
                    #[cfg(not(target_os = "windows"))]
                    let _ = (delta_x, delta_y);
                    #[cfg(target_os = "windows")]
                    if delta_y == 0.0
                        && let Some((button, action)) = hooks
                            .try_read()
                            .ok()
                            .and_then(|maps| rebound_thumbwheel_action(&maps, delta_x))
                    {
                        info!(button = %button, action = %action.label(), "native thumb wheel → executing bound action");
                        if try_queue_action(&action_tx, action) {
                            return EventDisposition::Suppress;
                        }
                    }
                    EventDisposition::PassThrough
                }
            }
        }
        // Function-key remapper: on key-down, look up a [keyboard.bindings]
        // entry for this keycode + modifier mask. A match queues its action
        // (suppressing the original key so it doesn't also type / trigger its
        // native function); an unmatched key passes through untouched. Key-up
        // is ignored to avoid double-firing the action.
        HookEvent::Key(openlogi_hook::KeyEvent {
            keycode,
            pressed,
            modifiers,
        }) => {
            if !pressed {
                return EventDisposition::PassThrough;
            }
            let trigger = KeyTrigger {
                keycode,
                modifiers: convert_modifiers(modifiers),
            };
            match keyboard_bindings
                .try_read()
                .ok()
                .and_then(|m| m.get(&trigger).cloned())
            {
                Some(action) => {
                    info!(keycode, action = %action.label(), "key → executing bound action");
                    if try_queue_action(&action_tx, action) {
                        EventDisposition::Suppress
                    } else {
                        EventDisposition::PassThrough
                    }
                }
                None => EventDisposition::PassThrough,
            }
        }
    });

    match result {
        Ok(hook) => {
            info!("OS input hook installed");
            Some(hook)
        }
        Err(e) => {
            warn!(error = %e, "could not install OS input hook — events will not be captured");
            None
        }
    }
}

/// Resolve a native horizontal-wheel tick to a rebound thumb-wheel action.
/// The built-in horizontal-scroll defaults intentionally return `None` so the
/// physical wheel stays native unless the user changed that direction. On
/// Windows/MX Master 2S, positive `WM_MOUSEHWHEEL` delta is the physical
/// backward/down direction, so it maps to `ThumbwheelScrollDown`.
#[cfg(any(target_os = "windows", test))]
fn rebound_thumbwheel_action(maps: &HookMaps, delta_x: f32) -> Option<(ButtonId, Action)> {
    let button = if delta_x > 0.0 {
        ButtonId::ThumbwheelScrollDown
    } else if delta_x < 0.0 {
        ButtonId::ThumbwheelScrollUp
    } else {
        return None;
    };
    let action = maps.bindings.get(&button)?.clone();
    (action != default_binding(button)).then_some((button, action))
}

/// The action a gesture button's plain (no-swipe) click should fire: its
/// explicit [`GestureDirection::Click`] entry — honoring an explicit
/// [`Action::None`] ("Do Nothing") — or the button's [`default_binding`] when
/// the gesture map has no `Click` key (a sparse / hand-edited map, or the button
/// left the gesture set mid-hold). The fallback guarantees a gesture button's
/// suppressed press is never swallowed into nothing.
fn resolve_gesture_click(
    gestures: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    id: ButtonId,
) -> Action {
    gestures
        .get(&id)
        .and_then(|m| m.get(&GestureDirection::Click).cloned())
        .unwrap_or_else(|| default_binding(id))
}

/// Whether `action` is just `id`'s own native event — i.e. the button is mapped
/// to the very click (or extra-button press) it already produces. In that case
/// the hook should pass the event through to the OS rather than suppress and
/// re-synthesise it. For Back/Forward this keeps the genuine hardware button
/// 4/5 intact instead of round-tripping it through synthesis.
fn is_native_click(id: ButtonId, action: &Action) -> bool {
    matches!(
        (id, action),
        (ButtonId::LeftClick, Action::LeftClick)
            | (ButtonId::RightClick, Action::RightClick)
            | (ButtonId::MiddleClick, Action::MiddleClick)
            | (ButtonId::Back, Action::MouseBack)
            | (ButtonId::Forward, Action::MouseForward)
    )
}

/// Minimum time between two BrowserBack (or two BrowserForward) keyboard
/// dispatches, shared across the CGEventTap hook and the HID++ gesture
/// watcher — both call [`dispatch_action`] independently, and on devices
/// where one physical press is visible through both paths, a naive dispatch
/// would fire the keyboard shortcut twice for one click. Same window as the
/// HID++ path's own intra-press debounce (`BACK_FORWARD_DEBOUNCE` in
/// `openlogi-hid`), for consistency.
const BROWSER_NAV_DEBOUNCE: Duration = Duration::from_millis(150);

/// Per-direction last-dispatch timestamps backing [`browser_nav_debounce_ok`].
/// `(last_back, last_forward)`.
static BROWSER_NAV_LAST: Mutex<(Option<Instant>, Option<Instant>)> = Mutex::new((None, None));

/// Whether a BrowserBack/BrowserForward keyboard dispatch for `action` should
/// proceed, or be suppressed as a duplicate of one already sent (from either
/// dispatch path) within [`BROWSER_NAV_DEBOUNCE`]. Records the dispatch time
/// on every `true` return so the *next* call — from either path — sees it.
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
    let fire = slot.is_none_or(|t| now.duration_since(t) >= BROWSER_NAV_DEBOUNCE);
    if fire {
        *slot = Some(now);
    }
    fire
}

/// Route a bound action either to OS-level event synthesis
/// ([`openlogi_inject::execute`]) or to one of OpenLogi's hardware-side
/// handlers.
///
/// `dpi_cycle` is held across a write lock long enough to advance the index
/// and snapshot the new DPI + target; the actual HID write spawns its own
/// thread via [`write_dpi_in_background`] to keep event callbacks non-blocking.
/// `registry` confirms that `capture` is still current or supplies the current
/// inventory channel. Hardware actions are skipped when standalone callers do
/// not provide a registry.
pub fn dispatch_action(
    action: &Action,
    dpi_cycle: &Arc<RwLock<DpiCycles>>,
    device_key: Option<&str>,
    capture: &CaptureChannel,
    registry: Option<&ChannelRegistry>,
    receiver_access: &ReceiverAccess,
) {
    let next = match action {
        Action::CycleDpiPresets => match dpi_cycle.write() {
            Ok(mut guard) => guard.state_for(device_key).and_then(DpiCycleState::cycle),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                None
            }
        },
        Action::SetDpiPreset(i) => match dpi_cycle.write() {
            Ok(mut guard) => guard
                .state_for(device_key)
                .and_then(|state| state.set(usize::from(*i))),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — set skipped");
                None
            }
        },
        Action::ToggleSmartShift => {
            let target = dpi_cycle.read().ok().and_then(|g| g.target_for(device_key));
            info!("SmartShift toggle → flipping wheel mode");
            if let Some(registry) = registry {
                toggle_smartshift_in_background(capture, registry, receiver_access, target);
            } else {
                warn!("no inventory registry — SmartShift toggle skipped");
            }
            return;
        }
        // BrowserBack/BrowserForward fall through to the keyboard shortcut
        // (Cmd+[ / Cmd+]) here — for Chrome and other apps that respond to
        // it, and as the HID++ gesture watcher's own fallback when its
        // AXPress attempt (Safari) fails. On devices where one physical press
        // is visible through both the CGEventTap hook and the HID++ diverted
        // path (e.g. MX Vertical), both independently reach this arm for the
        // *same* press, so it's cross-path debounced — otherwise a
        // keyboard-driven browser like Chrome would navigate twice per click.
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
        if let Some(registry) = registry {
            write_dpi_in_background(capture, registry, receiver_access, target, dpi);
        } else {
            warn!("no inventory registry — DPI action skipped");
        }
    } else if matches!(action, Action::CycleDpiPresets | Action::SetDpiPreset(_)) {
        info!(
            action = %action.label(),
            "no DPI presets configured for active device — press ignored"
        );
    }
}

#[cfg(test)]
mod tests;
