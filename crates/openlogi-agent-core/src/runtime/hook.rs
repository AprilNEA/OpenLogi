//! OS-hook capture and gesture interpretation.
//!
//! Installs the platform hook lazily, reads atomically published button maps,
//! and converts callback-thread mouse/key input into the shared action runtime.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    Action, Binding, ButtonId, GestureDirection, SwipeAccumulator, default_binding,
};
use openlogi_core::config::KeyTrigger;
use openlogi_hook::{
    EventDevice, EventDisposition, Hook, HookEvent, KeyEvent, MouseEvent, source_is_remappable,
};
use tracing::{info, warn};

use super::scroll::ScrollInputHandle;
use super::{ActionDispatchTarget, ActionDispatcher, PressToken};
use crate::event_monitor::SharedEventMonitor;

/// The button maps and selected-device thumb-wheel polarity the OS-hook callback
/// reads, kept behind ONE lock so a config rebuild publishes one coherent
/// snapshot. A callback during a device/app switch can never combine one
/// device's bindings with another device's direction convention.
#[derive(Default)]
pub struct HookMaps {
    /// Per-button immediate or threshold binding — the non-gesture dispatch path.
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Per-direction maps for the OS-hook gesture buttons (Back/Forward in
    /// gesture mode), so a hold+swipe resolves to a bound action. HID++
    /// gesture sources use the gesture watcher's separate map instead.
    pub gestures: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// The pointer identity that selected this snapshot, or focused policy.
    pub(crate) pointer_target: Option<openlogi_hook::PointerTarget>,
    /// Device whose binding maps this snapshot contains.
    #[cfg_attr(
        not(any(target_os = "windows", test)),
        expect(dead_code, reason = "read only by the Windows native-wheel fallback")
    )]
    pub(crate) selected_device: Option<String>,
    /// Per-device `0x2150 default_dir`, learned by HID++ capture sessions:
    /// `true` means a positive native horizontal delta is physical forward/up.
    /// Entries survive map rebuilds because they are hardware observations,
    /// not configuration.
    pub(crate) thumbwheel_positive_is_forward: BTreeMap<String, bool>,
}

/// Shared, atomically-published [`HookMaps`], threaded between the config owner
/// (orchestrator), the OS-hook callback, and the gesture watcher.
pub type SharedHookMaps = Arc<RwLock<HookMaps>>;

/// Shared keyboard trigger→action map for the function-key remapper. Unlike
/// mouse bindings these are not per-app-profile (M1 scope — per the spec's
/// non-goals), so a single map suffices. Keyed by the config `KeyTrigger`
/// (keycode + modifiers).
pub type SharedKeyboardBindings = Arc<RwLock<BTreeMap<KeyTrigger, Action>>>;

/// Tracks which OS-hook gesture button (Back/Forward) is mid-hold and defers the
/// swipe detection itself to a shared [`SwipeAccumulator`], which commits a swipe
/// *mid-motion* like the HID++ gesture-button path in `openlogi-hid`. This wrapper
/// adds only the button identity the accumulator doesn't track; a press that
/// never commits a direction is a plain click, fired on release.
/// A gesture hold this old is presumed stale — real hold+swipe interactions
/// finish in well under a second, and only a lost button-up (with no OS
/// interrupt to trigger [`HoldState::cancel`]) leaves one lingering.
const HOLD_STALE_AFTER: Duration = Duration::from_secs(10);

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
    /// aged past [`HOLD_STALE_AFTER`], without which every other gesture button
    /// would stay refused indefinitely.
    fn prepare_begin(&mut self, button: ButtonId) -> HoldAdmission {
        let Some(held) = self.current.take() else {
            return HoldAdmission::Begin;
        };
        if held.button != button && held.started_at.elapsed() < HOLD_STALE_AFTER {
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
            && let Some(aged) = Instant::now().checked_sub(HOLD_STALE_AFTER)
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
    /// Accepted non-gesture presses retain their edge disposition even when
    /// moving the pointer changes the binding before physical release.
    static SUPPRESSED_PRESSES: RefCell<HashSet<ButtonId>> = RefCell::new(HashSet::new());
    /// Function keys whose held action owns an accepted lifecycle. Repeated
    /// key-down events are auto-repeat, not replacement presses; their first
    /// matching key-up ends the lifecycle.
    static HELD_KEYS: RefCell<HashSet<u16>> = RefCell::new(HashSet::new());
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

/// Whether a wheel event may be replaced by host-side smooth output.
///
/// Native trackpad/pixel gestures always stay untouched. macOS additionally
/// requires a known Logitech sender; Linux and Windows perform device
/// selection before this callback and therefore admit their unattributed
/// wheel events through the same policy as button remapping.
fn scroll_source_may_intercept(from_trackpad: bool, device: Option<&EventDevice>) -> bool {
    !from_trackpad && button_source_may_remap(device)
}

/// Off-thread worker for bound actions so the tap callback never injects input.
type QueuedAction = (Action, ActionDispatchTarget);

fn spawn_action_worker(dispatcher: ActionDispatcher) -> mpsc::SyncSender<QueuedAction> {
    let (tx, rx) = mpsc::sync_channel::<QueuedAction>(64);
    let _ = thread::Builder::new()
        .name("openlogi-action".into())
        .spawn(move || {
            while let Ok((action, target)) = rx.recv() {
                dispatcher.executor.dispatch_to(&action, None, target);
            }
        });
    tx
}

/// Queue a bound action without blocking the tap callback. Returns `false` if
/// the queue is full (caller should fail open and pass the physical event).
fn try_queue_action(
    tx: &mpsc::SyncSender<QueuedAction>,
    action: Action,
    target: ActionDispatchTarget,
) -> bool {
    if tx.try_send((action, target)).is_err() {
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
    capture_target: impl FnOnce() -> ActionDispatchTarget,
) -> EventDisposition {
    // Primary L/R always pass through (suppressing them would brick the mouse).
    if !id.is_os_hook_button() || !button_source_may_remap(device) {
        return EventDisposition::PassThrough;
    }
    // `try_read` only: a blocking read on the tap thread freezes every pointer
    // event while a config rebuild holds the write lock. Fail open if unavailable.
    let (binding, is_gesture, pointer_target) =
        hooks.try_read().map_or((None, false, None), |maps| {
            (
                maps.bindings.get(&id).cloned(),
                maps.gestures.contains_key(&id),
                maps.pointer_target,
            )
        });
    let action_target = if pressed {
        pointer_target.map_or_else(capture_target, ActionDispatchTarget::Pointer)
    } else {
        ActionDispatchTarget::Keyboard
    };
    if pressed {
        // A refused begin — a second gesture button pressed mid-hold — falls
        // through to the single-action path: the first hold wins and this press
        // still means its plain click.
        let admission = is_gesture.then(|| HOLD.with_borrow_mut(|h| h.prepare_begin(id)));
        if let Some(HoldAdmission::Begin | HoldAdmission::Replace(_)) = &admission {
            if let Some(HoldAdmission::Replace(stale)) = &admission {
                dispatcher.cancel_stale_hook_press(stale);
            }
            if let Some(press) = dispatcher.try_hook_button_down(id, None, action_target) {
                HOLD.with_borrow_mut(|h| h.begin(id, press));
                return EventDisposition::Suppress;
            }
            return SUPPRESSED_PRESSES
                .with_borrow_mut(|s| remapped_press_disposition(id, false, s));
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
        let disposition =
            SUPPRESSED_PRESSES.with_borrow_mut(|s| remapped_release_disposition(id, s));
        if disposition == EventDisposition::Suppress {
            dispatcher.try_hook_button_up(id);
        }
        return disposition;
    }

    let Some(binding) = binding else {
        return SUPPRESSED_PRESSES.with_borrow_mut(|s| remapped_press_disposition(id, false, s));
    };
    if binding_is_native_click(id, &binding) {
        return SUPPRESSED_PRESSES.with_borrow_mut(|s| remapped_press_disposition(id, false, s));
    }
    info!(button = %id, action = %binding.click_action().label(), "button → handling binding");
    let queued = dispatcher
        .try_hook_button_down(id, Some(&binding), action_target)
        .is_some();
    SUPPRESSED_PRESSES.with_borrow_mut(|s| remapped_press_disposition(id, queued, s))
}

fn binding_is_native_click(id: ButtonId, binding: &Binding) -> bool {
    !matches!(binding, Binding::LongPress(_)) && is_native_click(id, &binding.click_action())
}

/// Press of a remapped single-action button: suppress when the action was
/// queued, and remember that decision so release uses the same disposition.
fn remapped_press_disposition(
    id: ButtonId,
    queued: bool,
    suppressed: &mut HashSet<ButtonId>,
) -> EventDisposition {
    if queued {
        suppressed.insert(id);
        EventDisposition::Suppress
    } else {
        suppressed.remove(&id);
        EventDisposition::PassThrough
    }
}

/// A release follows its matching press, not the pointer's current profile.
fn remapped_release_disposition(
    id: ButtonId,
    suppressed: &mut HashSet<ButtonId>,
) -> EventDisposition {
    if suppressed.remove(&id) {
        EventDisposition::Suppress
    } else {
        EventDisposition::PassThrough
    }
}

/// Suppress only input accepted by an off-thread runtime. Rejected input must
/// fail open so the hook never swallows an edge it could not dispatch.
fn queued_event_disposition(queued: bool) -> EventDisposition {
    if queued {
        EventDisposition::Suppress
    } else {
        EventDisposition::PassThrough
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

/// Remap one function-key edge without blocking the hook callback.
fn handle_key(
    event: KeyEvent,
    bindings: &SharedKeyboardBindings,
    action_tx: &mpsc::SyncSender<QueuedAction>,
    dispatcher: &ActionDispatcher,
    capture_target: impl FnOnce() -> ActionDispatchTarget,
) -> EventDisposition {
    let KeyEvent {
        keycode,
        pressed,
        modifiers,
    } = event;
    if !pressed {
        return HELD_KEYS.with_borrow_mut(|keys| {
            if keys.remove(&keycode) {
                queued_event_disposition(dispatcher.try_hook_key_up(keycode))
            } else {
                EventDisposition::PassThrough
            }
        });
    }
    if HELD_KEYS.with_borrow(|keys| keys.contains(&keycode)) {
        return EventDisposition::Suppress;
    }
    let trigger = KeyTrigger { keycode, modifiers };
    let Some(action) = bindings
        .try_read()
        .ok()
        .and_then(|map| map.get(&trigger).cloned())
    else {
        return EventDisposition::PassThrough;
    };

    info!(keycode, action = %action.label(), "key → executing bound action");
    let action_target = capture_target();
    let queued = if action.held_combo().is_some() {
        let queued = dispatcher.try_hook_key_down(keycode, &action, action_target);
        if queued {
            HELD_KEYS.with_borrow_mut(|keys| {
                keys.insert(keycode);
            });
        }
        queued
    } else {
        try_queue_action(action_tx, action, action_target)
    };
    queued_event_disposition(queued)
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    hooks: SharedHookMaps,
    keyboard_bindings: SharedKeyboardBindings,
    dispatcher: ActionDispatcher,
    scroll: ScrollInputHandle,
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
                } => handle_button(
                    id,
                    pressed,
                    device.as_ref(),
                    &hooks,
                    &dispatcher,
                    ActionDispatchTarget::capture,
                ),
                MouseEvent::Moved { delta_x, delta_y } => {
                    handle_moved(delta_x, delta_y, &hooks, &dispatcher)
                }
                MouseEvent::CaptureInterrupted => {
                    HOLD.with_borrow_mut(HoldState::cancel);
                    HELD_KEYS.with_borrow_mut(HashSet::clear);
                    dispatcher.cancel_hook_thread_buttons();
                    scroll.cancel_hooks();
                    EventDisposition::PassThrough
                }
                MouseEvent::Scroll {
                    delta,
                    from_trackpad,
                    device,
                } => {
                    #[cfg(target_os = "windows")]
                    if delta.y() == 0.0
                        && let Some((button, action, target)) =
                            hooks.try_read().ok().and_then(|maps| {
                                rebound_thumbwheel_action(&maps, delta.x())
                                    .map(|(button, action)| (button, action, maps.pointer_target))
                            })
                    {
                        info!(button = %button, action = %action.label(), "native thumb wheel → executing bound action");
                        return queued_event_disposition(try_queue_action(
                            &action_tx,
                            action,
                            ActionDispatchTarget::for_pointer(target),
                        ));
                    }
                    if scroll_source_may_intercept(from_trackpad, device.as_ref()) {
                        return queued_event_disposition(scroll.try_hook_scroll(delta));
                    }
                    EventDisposition::PassThrough
                }
            }
        }
        // Function-key remapper: ordinary actions remain one-shot, while a
        // HoldShortcut enters the same down/up/cancel lifecycle as a mouse
        // button. The active set pairs key-up even if modifier state or config
        // changes while the key is down.
        HookEvent::Key(event) => handle_key(
            event,
            &keyboard_bindings,
            &action_tx,
            &dispatcher,
            ActionDispatchTarget::capture,
        ),
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
/// devices exposing `0x2150`, the learned `default_dir` determines which
/// physical direction the native delta represents. A selected device whose
/// polarity has not arrived yet fails open instead of guessing the opposite
/// action; only the legacy no-device-context path retains the MX Master 2S
/// fallback (positive is physical backward/down).
#[cfg(any(target_os = "windows", test))]
fn rebound_thumbwheel_action(maps: &HookMaps, delta_x: f64) -> Option<(ButtonId, Action)> {
    let positive_is_forward = match maps.selected_device.as_deref() {
        Some(key) => maps.thumbwheel_positive_is_forward.get(key).copied()?,
        None => false,
    };
    let forward = if delta_x > 0.0 {
        positive_is_forward
    } else if delta_x < 0.0 {
        !positive_is_forward
    } else {
        return None;
    };
    let button = if forward {
        ButtonId::ThumbwheelScrollUp
    } else {
        ButtonId::ThumbwheelScrollDown
    };
    let action = maps.bindings.get(&button)?.click_action();
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

#[cfg(test)]
mod tests;
