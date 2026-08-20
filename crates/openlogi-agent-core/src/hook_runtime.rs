//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
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

use crate::event_monitor::SharedEventMonitor;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};
use crate::receiver_access::ReceiverAccess;
use crate::side_gesture::{SharedSideGesture, SideGestureAction};
use crate::{DpiCycleState, DpiCycles};

/// Runtime dependencies shared by every action source: the OS hook, HID++
/// controls, keyboard capture, and Actions Ring slot activation.
#[derive(Clone)]
pub struct ActionDispatcher {
    dpi_cycle: Arc<RwLock<DpiCycles>>,
    capture: CaptureChannel,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    action_ring: tokio::sync::mpsc::UnboundedSender<Option<String>>,
}

impl ActionDispatcher {
    /// Build a dispatcher around the agent's shared device state and ring queue.
    #[must_use]
    pub fn new(
        dpi_cycle: Arc<RwLock<DpiCycles>>,
        capture: CaptureChannel,
        registry: ChannelRegistry,
        receiver_access: ReceiverAccess,
        action_ring: tokio::sync::mpsc::UnboundedSender<Option<String>>,
    ) -> Self {
        Self {
            dpi_cycle,
            capture,
            registry,
            receiver_access,
            action_ring,
        }
    }

    /// Route one action without blocking the input callback.
    pub fn dispatch(&self, action: &Action, device_key: Option<&str>) {
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
    /// The held button and when its hold began. The timestamp exists solely
    /// for stale-hold recovery in [`Self::begin`].
    button: Option<(ButtonId, Instant)>,
    swipe: SwipeAccumulator,
}

impl HoldState {
    /// Begin a hold for `button`, unless another button's live hold is in
    /// progress — with several gesture buttons the first hold wins, so a second
    /// press can't hijack the accumulated motion mid-swipe. Returns whether the
    /// hold started (the caller lets a refused press fall through to the
    /// single-action path, where it means its plain click).
    ///
    /// Two presses recover a hold whose button-up was lost (nothing else ever
    /// clears it when the OS drops a release without an interrupt): a re-press
    /// of the held button itself — a button cannot be pressed while down, so
    /// this is proof the release was lost — and any press once the hold has
    /// aged past [`STALE_HOLD`], without which every other gesture button
    /// would stay refused indefinitely.
    fn begin(&mut self, button: ButtonId) -> bool {
        if let Some((held, since)) = self.button
            && held != button
            && since.elapsed() < STALE_HOLD
        {
            return false;
        }
        self.button = Some((button, Instant::now()));
        self.swipe.begin();
        true
    }

    /// Feed a pointer-move delta into the active hold, tagging a committed swipe
    /// with the held button. Returns `Some((button, direction))` exactly once per
    /// hold, or `None` while still too short, already fired, or not holding.
    fn accumulate(&mut self, dx: i32, dy: i32) -> Option<(ButtonId, GestureDirection)> {
        let (button, _) = self.button?;
        self.swipe.accumulate(dx, dy).map(|dir| (button, dir))
    }

    /// End the hold for `button`. Returns `Some(true)` when it ended a hold that
    /// never committed a swipe (the caller should fire the `Click` action),
    /// `Some(false)` when a swipe already fired, and `None` for a stray release
    /// of a button we weren't holding.
    fn end(&mut self, button: ButtonId) -> Option<bool> {
        if self.button.is_some_and(|(held, _)| held == button) {
            self.button = None;
            Some(self.swipe.end())
        } else {
            None
        }
    }

    /// Cancel any in-progress hold without firing anything — used when the OS
    /// interrupts capture. A dropped button-up would otherwise leave a stale hold
    /// that the next stray pointer move turns into a phantom swipe.
    fn cancel(&mut self) {
        self.button = None;
        self.swipe.end();
    }

    /// Age the current hold past the staleness horizon, so tests can exercise
    /// the lost-button-up recovery without sleeping.
    #[cfg(test)]
    fn backdate_for_test(&mut self) {
        if let Some((_, since)) = &mut self.button
            && let Some(aged) = Instant::now().checked_sub(STALE_HOLD)
        {
            *since = aged;
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
/// macOS fails closed because its hook is global: only a known Logitech,
/// non-trackpad source may be suppressed. Bluetooth-direct Back/Forward
/// gestures are captured through their device-specific HID++ session instead
/// of weakening this policy. Linux/Windows restrict hook attachment upstream,
/// so an unavailable source remains eligible there.
fn button_source_may_remap(device: Option<&EventDevice>) -> bool {
    match device {
        Some(d) => source_is_remappable(Some(d)),
        // Linux/Windows restrict which devices the hook attaches to upstream.
        // macOS uses one global tap, so an unattributed event must fail closed.
        None => !cfg!(target_os = "macos"),
    }
}

struct QueuedAction {
    action: Action,
    device_key: Option<String>,
}

/// Off-thread worker for bound actions so the tap callback never injects input.
fn spawn_action_worker(dispatcher: ActionDispatcher) -> mpsc::SyncSender<QueuedAction> {
    let (tx, rx) = mpsc::sync_channel::<QueuedAction>(64);
    let _ = thread::Builder::new()
        .name("openlogi-action".into())
        .spawn(move || {
            while let Ok(queued) = rx.recv() {
                dispatcher.dispatch(&queued.action, queued.device_key.as_deref());
            }
        });
    tx
}

/// Queue a bound action without blocking the tap callback. Returns `false` if
/// the queue is full (caller should fail open and pass the physical event).
fn try_queue_action(
    tx: &mpsc::SyncSender<QueuedAction>,
    action: Action,
    device_key: Option<String>,
) -> bool {
    if tx.try_send(QueuedAction { action, device_key }).is_err() {
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
    action_tx: &mpsc::SyncSender<QueuedAction>,
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
        if is_gesture && HOLD.with_borrow_mut(|h| h.begin(id)) {
            return EventDisposition::Suppress;
        }
    } else {
        // Drop the HOLD borrow before any queueing (re-entrancy freeze hazard).
        let ended = HOLD.with_borrow_mut(|h| h.end(id));
        if let Some(was_click) = ended {
            if was_click {
                let action = hooks
                    .try_read()
                    .ok()
                    .map(|m| resolve_gesture_click(&m.gestures, id));
                if let Some(action) = action {
                    info!(button = %id, action = %action.label(), "gesture click → executing bound action");
                    let _ = try_queue_action(action_tx, action, None);
                }
            }
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
        let queued = try_queue_action(action_tx, action, None);
        return FAIL_OPEN_PRESSES.with_borrow_mut(|s| remapped_press_disposition(id, queued, s));
    }
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
    side_gesture: &SharedSideGesture,
    action_tx: &mpsc::SyncSender<QueuedAction>,
) -> EventDisposition {
    let commit = HOLD.with_borrow_mut(|h| h.accumulate(delta_x, delta_y));
    if let Some((button, dir)) = commit {
        let action = hooks.try_read().ok().map(|m| {
            m.gestures
                .get(&button)
                .and_then(|dirs| dirs.get(&dir).cloned())
                .unwrap_or_else(|| resolve_gesture_click(&m.gestures, button))
        });
        if let Some(action) = action {
            info!(button = %button, ?dir, action = %action.label(), "gesture swipe → executing bound action");
            let _ = try_queue_action(action_tx, action, None);
        }
    }
    let device_commit = side_gesture.try_accumulate(delta_x, delta_y);
    if let Some(SideGestureAction {
        device_key,
        button,
        direction,
        action,
    }) = device_commit
    {
        info!(key = device_key, %button, ?direction, action = %action.label(), "HID++ side gesture swipe → executing bound action");
        let _ = try_queue_action(action_tx, action, Some(device_key));
    }
    EventDisposition::PassThrough
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    hooks: SharedHookMaps,
    keyboard_bindings: SharedKeyboardBindings,
    side_gesture: SharedSideGesture,
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
    let action_tx = spawn_action_worker(dispatcher);

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
                } => handle_button(id, pressed, device.as_ref(), &hooks, &action_tx),
                MouseEvent::Moved { delta_x, delta_y } => {
                    handle_moved(delta_x, delta_y, &hooks, &side_gesture, &action_tx)
                }
                MouseEvent::CaptureInterrupted => {
                    HOLD.with_borrow_mut(HoldState::cancel);
                    side_gesture.interrupt();
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
                        if try_queue_action(&action_tx, action, None) {
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
                    if try_queue_action(&action_tx, action, None) {
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
mod tests {
    use super::*;
    use openlogi_core::binding::GESTURE_SWIPE_THRESHOLD;

    #[test]
    fn senderless_buttons_follow_the_platform_source_policy() {
        assert_eq!(button_source_may_remap(None), !cfg!(target_os = "macos"));
    }

    #[test]
    fn attributed_sources_still_follow_the_device_policy() {
        let trackpad = EventDevice {
            product_name: Some("Apple Internal Keyboard / Trackpad".into()),
            ..EventDevice::default()
        };
        let logitech_mouse = EventDevice {
            product_name: Some("Logitech MX Master 3".into()),
            ..EventDevice::default()
        };

        assert!(!button_source_may_remap(Some(&trackpad)));
        assert!(button_source_may_remap(Some(&logitech_mouse)));
    }

    // The mid-swipe gate itself is unit-tested on `SwipeAccumulator` in
    // `openlogi-core`; these cover only what `HoldState` adds on top — tagging a
    // commit with the held button, and matching the button on release.

    #[test]
    fn accumulate_tags_a_committed_swipe_with_the_held_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        hold.swipe.backdate_hold_for_test();

        // A clear rightward swipe commits, tagged with the held button.
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some((ButtonId::Back, GestureDirection::Right))
        );
        assert_eq!(
            hold.accumulate(50, 0),
            None,
            "commits at most once per hold"
        );
        // A release after a committed swipe is NOT a click.
        assert_eq!(hold.end(ButtonId::Back), Some(false));
    }

    #[test]
    fn a_same_button_re_press_restarts_the_stale_hold() {
        // A press for the very button we think is held can only mean its
        // release was lost (a button cannot be pressed while down): the hold
        // restarts instead of wedging on the stale state.
        let mut hold = HoldState::default();
        assert!(hold.begin(ButtonId::Back));
        assert!(
            hold.begin(ButtonId::Back),
            "a same-button re-press is proof of a lost release"
        );
        hold.swipe.backdate_hold_for_test();
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some((ButtonId::Back, GestureDirection::Right))
        );
    }

    #[test]
    fn an_aged_hold_yields_to_a_new_buttons_press() {
        // No release ever clears a hold whose button-up was lost (and no OS
        // interrupt fired), so a different gesture button's press takes over
        // once the hold is old enough to be presumed stale — otherwise every
        // gesture button stays wedged until the stale one is pressed again.
        let mut hold = HoldState::default();
        assert!(hold.begin(ButtonId::Back));
        hold.backdate_for_test();
        assert!(
            hold.begin(ButtonId::Forward),
            "an aged hold must yield to a new press"
        );
        hold.swipe.backdate_hold_for_test();
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some((ButtonId::Forward, GestureDirection::Right))
        );
    }

    #[test]
    fn begin_is_first_wins_while_a_hold_is_active() {
        // Two gesture buttons pressed together: the first hold keeps the
        // accumulator; the second press is refused (its caller falls through to
        // the single-action path) and its release is a stray, not a click.
        let mut hold = HoldState::default();
        assert!(hold.begin(ButtonId::Back));
        hold.swipe.backdate_hold_for_test();
        assert!(
            !hold.begin(ButtonId::Forward),
            "a second press must not hijack the active hold"
        );

        // The accumulated motion still belongs to the first button...
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            Some((ButtonId::Back, GestureDirection::Right))
        );
        // ...the refused button's release is a stray...
        assert_eq!(hold.end(ButtonId::Forward), None);
        // ...and the first hold ends normally (swipe fired, so not a click).
        assert_eq!(hold.end(ButtonId::Back), Some(false));
    }

    #[test]
    fn end_matches_the_held_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back);
        // A stray release of a button we weren't holding is ignored...
        assert_eq!(hold.end(ButtonId::Forward), None);
        // ...and ending the held button with no swipe is a plain click.
        assert_eq!(hold.end(ButtonId::Back), Some(true));
    }

    #[test]
    fn resolve_gesture_click_prefers_explicit_then_falls_back_to_default() {
        // Explicit Click action wins.
        let gestures = BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Click, Action::Copy)]),
        )]);
        assert_eq!(
            resolve_gesture_click(&gestures, ButtonId::Back),
            Action::Copy
        );

        // Explicit `Click = None` ("Do Nothing") is respected, NOT overridden by
        // the default — the button intentionally does nothing on a plain click.
        let off = BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Click, Action::None)]),
        )]);
        assert_eq!(resolve_gesture_click(&off, ButtonId::Back), Action::None);
    }

    #[test]
    fn fail_open_press_pairs_release() {
        let mut fail_open = HashSet::new();
        // Queue accepted → suppress press and release.
        assert_eq!(
            remapped_press_disposition(ButtonId::Back, true, &mut fail_open),
            EventDisposition::Suppress
        );
        assert_eq!(
            remapped_release_disposition(ButtonId::Back, &mut fail_open),
            EventDisposition::Suppress
        );
        // Queue rejected → pass through press *and* matching release.
        assert_eq!(
            remapped_press_disposition(ButtonId::Forward, false, &mut fail_open),
            EventDisposition::PassThrough
        );
        assert_eq!(
            remapped_release_disposition(ButtonId::Forward, &mut fail_open),
            EventDisposition::PassThrough
        );
        // A later unpaired release of that button suppresses again.
        assert_eq!(
            remapped_release_disposition(ButtonId::Forward, &mut fail_open),
            EventDisposition::Suppress
        );
    }

    #[test]
    fn rebound_horizontal_wheel_maps_to_thumbwheel_directions() {
        let maps = HookMaps {
            bindings: BTreeMap::from([
                (ButtonId::ThumbwheelScrollUp, Action::NextTab),
                (ButtonId::ThumbwheelScrollDown, Action::PrevTab),
            ]),
            gestures: BTreeMap::new(),
        };
        assert_eq!(
            rebound_thumbwheel_action(&maps, 1.0),
            Some((ButtonId::ThumbwheelScrollDown, Action::PrevTab))
        );
        assert_eq!(
            rebound_thumbwheel_action(&maps, -1.0),
            Some((ButtonId::ThumbwheelScrollUp, Action::NextTab))
        );
        assert_eq!(rebound_thumbwheel_action(&maps, 0.0), None);
    }

    #[test]
    fn native_thumbwheel_scroll_stays_os_native() {
        let maps = HookMaps {
            bindings: BTreeMap::from([
                (
                    ButtonId::ThumbwheelScrollUp,
                    default_binding(ButtonId::ThumbwheelScrollUp),
                ),
                (
                    ButtonId::ThumbwheelScrollDown,
                    default_binding(ButtonId::ThumbwheelScrollDown),
                ),
            ]),
            gestures: BTreeMap::new(),
        };
        assert_eq!(rebound_thumbwheel_action(&maps, 1.0), None);
        assert_eq!(rebound_thumbwheel_action(&maps, -1.0), None);
    }

    #[test]
    fn resolve_gesture_click_falls_back_when_click_is_absent() {
        // A gesture map with no Click key (sparse/hand-edited) falls back to the
        // button's default, so the suppressed press is never swallowed.
        let no_click = BTreeMap::from([(
            ButtonId::Back,
            BTreeMap::from([(GestureDirection::Up, Action::Copy)]),
        )]);
        assert_eq!(
            resolve_gesture_click(&no_click, ButtonId::Back),
            default_binding(ButtonId::Back)
        );

        // The button missing from the map entirely (e.g. removed by a config
        // reload mid-hold) also falls back to its default rather than nothing.
        let empty = BTreeMap::new();
        assert_eq!(
            resolve_gesture_click(&empty, ButtonId::Forward),
            default_binding(ButtonId::Forward)
        );
    }
}
