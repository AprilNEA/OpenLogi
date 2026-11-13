//! OS-hook capture and gesture interpretation.
//!
//! Installs the platform hook lazily, reads atomically published button maps,
//! and converts callback-thread mouse/key input into the shared action runtime.

mod diverted;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    Action, Binding, ButtonId, GestureDirection, SwipeAccumulator, default_binding,
};
use openlogi_core::config::KeyTrigger;
use openlogi_hid::DeviceRoute;
use openlogi_hook::{
    EventDevice, EventDisposition, Hook, HookEvent, KeyEvent, MouseEvent, source_is_remappable,
};
use tracing::{debug, info, warn};

use super::button::PressControl;
use super::scroll::ScrollInputHandle;
use super::{
    ActionDispatchTarget, ActionDispatcher, PointerPress, PointerSelection, PressToken,
    log_bound_action,
};
use crate::event_monitor::SharedEventMonitor;
use crate::flow::FlowInput;
pub(crate) use diverted::DivertedButtons;

/// The button maps and HID++ capture observations the OS-hook callback reads,
/// kept behind ONE lock so a config rebuild publishes one coherent snapshot. A
/// callback during a device/app switch can never combine one device's bindings
/// with another device's capture state.
#[derive(Default)]
pub struct HookMaps {
    /// Per-button immediate or threshold binding — the non-gesture dispatch path.
    pub bindings: BTreeMap<ButtonId, Binding>,
    /// Per-direction maps for the OS-hook gesture buttons (Middle/Back/Forward in
    /// gesture mode), so a hold+swipe resolves to a bound action. HID++
    /// gesture sources use the gesture watcher's separate map instead.
    pub gestures: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// The pointer context that selected this snapshot, or `None` for the
    /// focused policy.
    pub(crate) pointer: Option<PointerSelection>,
    /// Device whose binding maps this snapshot contains.
    pub(crate) selected_device: Option<String>,
    /// The HID++ route of `selected_device`, which names that device's
    /// capture sessions in `diverted_buttons`.
    pub(crate) selected_route: Option<DeviceRoute>,
    /// OS-hook buttons that live HID++ capture sessions have diverted,
    /// reported by the sessions themselves. These are hardware observations,
    /// not configuration, so they survive map rebuilds.
    pub(crate) diverted_buttons: DivertedButtons,
    /// The selected device's G-Shift bindings.
    pub(crate) shift_bindings: BTreeMap<ButtonId, Binding>,
    /// Devices with a G-Shift button held, set by HID++ capture. Survives map
    /// rebuilds like `diverted_buttons`.
    pub(crate) shift_held: BTreeSet<String>,
}

impl HookMaps {
    /// The held G-Shift layer's binding for `id`, which replaces the
    /// selected device's own binding and gestures for the press.
    fn shift_binding(&self, id: ButtonId) -> Option<&Binding> {
        self.selected_device
            .as_ref()
            .filter(|device| self.shift_held.contains(*device))
            .and_then(|_| self.shift_bindings.get(&id))
    }

    /// The binding for `id` and whether it is a gesture button, honouring a
    /// held G-Shift layer.
    fn press_binding(&self, id: ButtonId) -> (Option<Binding>, bool) {
        if let Some(binding) = self.shift_binding(id) {
            return (Some(binding.clone()), false);
        }
        (
            self.bindings.get(&id).cloned(),
            self.gestures.contains_key(&id),
        )
    }

    /// The pointer identity that a press of `id` carries, judged on the layer
    /// that [`Self::press_binding`] reads.
    fn pointer_press(&self, id: ButtonId) -> Option<PointerPress> {
        let pointer = self.pointer.as_ref()?;
        Some(if self.shift_binding(id).is_some() {
            pointer.unscoped_press()
        } else {
            pointer.press(id)
        })
    }

    /// What a live HID++ diversion of `id` says about a press from `source`.
    fn diverted_press(&self, id: ButtonId, source: Option<&EventDevice>) -> Option<DivertedPress> {
        match source {
            Some(source) => self
                .diverted_buttons
                .owns(source, id)
                .then_some(DivertedPress::Echo),
            None => self
                .selected_route
                .as_ref()
                .is_some_and(|route| self.diverted_buttons.diverted_on(route, id))
                .then_some(DivertedPress::OtherDevice),
        }
    }
}

/// A press of an OS-hook button that HID++ capture has taken over somewhere.
/// Both edges of the press get the same disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DivertedPress {
    /// The OS copy of a control a capture session diverted on the same device.
    /// The session dispatches that press, so the hook must not dispatch it
    /// again (#2158). It must not pass it through either: diversion already
    /// took the control out of the native report, so this copy reaching the
    /// desktop would be a second, unbound press.
    Echo,
    /// An unattributed press of a control the selected device has diverted.
    /// None of that device's presses of it reach the OS, so another mouse sent
    /// this one and the selected device's binding is not its own (#2381). It
    /// stays native.
    OtherDevice,
}

impl DivertedPress {
    fn disposition(self) -> EventDisposition {
        match self {
            Self::Echo => EventDisposition::Suppress,
            Self::OtherDevice => EventDisposition::PassThrough,
        }
    }
}

/// Shared, atomically-published [`HookMaps`], threaded between the config owner
/// (orchestrator), the OS-hook callback, and the gesture watcher.
pub type SharedHookMaps = Arc<RwLock<HookMaps>>;

/// Shared keyboard trigger→action map for the function-key remapper. Unlike
/// mouse bindings these are not per-app-profile (M1 scope — per the spec's
/// non-goals), so a single map suffices. Keyed by the config `KeyTrigger`
/// (keycode + modifiers).
pub type SharedKeyboardBindings = Arc<RwLock<BTreeMap<KeyTrigger, Action>>>;

/// A gesture hold this old is presumed stale — real hold+swipe interactions
/// finish in well under a second, and only a lost button-up (with no OS
/// interrupt to end it, see [`press_transition`]) leaves one lingering.
const HOLD_STALE_AFTER: Duration = Duration::from_secs(10);

/// A gesture button's (Middle/Back/Forward) press held for a swipe. The swipe
/// detection itself is the hold's own [`SwipeAccumulator`], which commits a
/// swipe *mid-motion* like the HID++ gesture-button path in `openlogi-hid`; a
/// hold that never commits a direction is a plain click, fired on release.
/// The accumulator begins and ends with the hold that owns it.
#[derive(Debug)]
struct GestureHold {
    started_at: Instant,
    press: PressToken,
    swipe: SwipeAccumulator,
}

impl GestureHold {
    /// Hold the press the accepted lifecycle `Down` returned.
    fn new(press: PressToken, now: Instant) -> Self {
        let mut swipe = SwipeAccumulator::default();
        swipe.begin();
        Self {
            started_at: now,
            press,
            swipe,
        }
    }

    /// End the hold, returning its token and any action still due on release.
    fn end(mut self) -> (PressToken, Option<GestureDirection>) {
        let direction = self.swipe.end();
        (self.press, direction)
    }
}

/// Where a press of an OS-hook button stands on its callback thread. A button
/// with no phase recorded is up. Both edges of a press get its phase's
/// [`disposition`](Self::disposition), so the hook and the desktop never split
/// a press: a release follows its press, not the bindings in force when it
/// arrives.
#[derive(Debug)]
enum PressPhase {
    /// A gesture press whose lifecycle the runtime accepted. At most one
    /// button per thread holds: a gesture press is first admitted by
    /// [`ButtonPresses::admit_gesture`], which refuses it while another
    /// button's hold is live and ends one that went stale.
    HeldAsGesture { hold: GestureHold },
    /// A remapped press whose lifecycle the runtime accepted. Its release is
    /// suppressed too, even when moving the pointer restores a native binding
    /// before the physical release.
    Suppressed,
    /// A press judged against a HID++ diversion. Its release follows the
    /// press, even if the capture session ends in between.
    Diverted(DivertedPress),
    /// A press delivered natively: unbound, bound to its own native click, or
    /// refused by the runtime.
    Passthrough,
}

impl PressPhase {
    fn disposition(&self) -> EventDisposition {
        match self {
            Self::HeldAsGesture { .. } | Self::Suppressed => EventDisposition::Suppress,
            Self::Diverted(diverted) => diverted.disposition(),
            Self::Passthrough => EventDisposition::PassThrough,
        }
    }
}

/// An input to one button's [`PressPhase`].
enum PressEvent {
    /// The button went down, and the hook gave the press this phase.
    Pressed(PressPhase),
    /// The button came up.
    Released,
    /// The OS dropped input, possibly a release with it.
    CaptureInterrupted,
}

/// What a [`PressEvent`] does to a button's recorded phase.
#[derive(Debug)]
enum NextPhase {
    /// The phase stays as recorded.
    Keep,
    /// The button is up; a release hands its press back for the I/O that ends it.
    Up,
    /// The button enters this phase.
    Enter(PressPhase),
}

/// The per-button press machine: what `event` does to a button whose recorded
/// phase is `phase` (`None`: up). Pure and total; [`ButtonPresses`] applies it
/// and the hook performs the I/O.
fn press_transition(phase: Option<&PressPhase>, event: PressEvent) -> NextPhase {
    match (phase, event) {
        // A button cannot be pressed while it is down, so a phase still
        // recorded here belongs to a press whose release was lost. The new
        // press replaces it. Kept beside the new press, a stale hold would
        // claim its release — suppressing it even when the press went native,
        // a stuck button — and turn its motion into phantom swipes.
        (_, PressEvent::Pressed(next)) => NextPhase::Enter(next),
        // A release ends its press. An interrupt ends a hold: it may have
        // dropped the hold's release, and a hold left behind turns the next
        // stray pointer move into a phantom swipe.
        (_, PressEvent::Released)
        | (Some(PressPhase::HeldAsGesture { .. }), PressEvent::CaptureInterrupted) => NextPhase::Up,
        // Any other press keeps its disposition for a release that still comes.
        (_, PressEvent::CaptureInterrupted) => NextPhase::Keep,
    }
}

/// The admission of a gesture press to its thread's one hold.
enum HoldAdmission {
    Begin,
    /// Begin after ending another button's stale hold, whose press this is.
    Replace(PressToken),
    Refuse,
}

/// The [`PressPhase`] of every OS-hook button down on one callback thread.
#[derive(Default)]
struct ButtonPresses {
    phases: HashMap<ButtonId, PressPhase>,
}

impl ButtonPresses {
    /// Record a press of `id` in `phase`, returning the phase it displaced,
    /// which only a lost release leaves behind.
    fn press(&mut self, id: ButtonId, phase: PressPhase) -> Option<PressPhase> {
        self.apply(id, PressEvent::Pressed(phase))
    }

    /// Release `id`, returning the press it ends, or `None` for a stray release.
    fn release(&mut self, id: ButtonId) -> Option<PressPhase> {
        self.apply(id, PressEvent::Released)
    }

    /// The OS interrupted capture: a hold ends without firing anything.
    fn interrupt(&mut self) {
        self.phases.retain(|_, phase| {
            matches!(
                press_transition(Some(phase), PressEvent::CaptureInterrupted),
                NextPhase::Keep
            )
        });
    }

    /// Apply `event` to `id`, returning the phase it ended or displaced.
    fn apply(&mut self, id: ButtonId, event: PressEvent) -> Option<PressPhase> {
        match press_transition(self.phases.get(&id), event) {
            NextPhase::Keep => None,
            NextPhase::Up => self.phases.remove(&id),
            NextPhase::Enter(phase) => self.phases.insert(id, phase),
        }
    }

    /// Admit a gesture press of `id` to the hold. With several gesture buttons
    /// the first live hold wins, so a second button cannot hijack accumulated
    /// motion. The caller obtains a fresh [`PressToken`] only after this
    /// admission step.
    ///
    /// Two presses recover a hold whose button-up was lost (nothing else ever
    /// clears it when the OS drops a release without an interrupt): a re-press
    /// of the held button itself, whose new phase replaces the hold, and any
    /// press once the hold has aged past [`HOLD_STALE_AFTER`], without which
    /// every other gesture button would stay refused indefinitely.
    fn admit_gesture(&mut self, id: ButtonId, now: Instant) -> HoldAdmission {
        let other = self.phases.iter().find_map(|(&button, phase)| match phase {
            PressPhase::HeldAsGesture { hold } if button != id => Some((button, hold)),
            _ => None,
        });
        let Some((button, hold)) = other else {
            return HoldAdmission::Begin;
        };
        if now.saturating_duration_since(hold.started_at) < HOLD_STALE_AFTER {
            return HoldAdmission::Refuse;
        }
        let stale = hold.press.clone();
        self.phases.remove(&button);
        HoldAdmission::Replace(stale)
    }

    /// Feed a pointer-move delta into the hold, tagging a committed swipe with
    /// its exact press token and held button. Returns `None` while still too
    /// short, already fired, or not holding.
    fn accumulate(&mut self, dx: i32, dy: i32) -> Option<(PressToken, ButtonId, GestureDirection)> {
        let (button, hold) = self
            .phases
            .iter_mut()
            .find_map(|(&button, phase)| match phase {
                PressPhase::HeldAsGesture { hold } => Some((button, hold)),
                _ => None,
            })?;
        hold.swipe
            .accumulate(dx, dy)
            .map(|dir| (hold.press.clone(), button, dir))
    }
}

thread_local! {
    /// The OS-hook button presses of one hook-callback thread: the single
    /// macOS tap thread, or — on Linux — one thread per device, so two mice
    /// never share a hold (a press on one can't hijack the other's swipe).
    /// Thread-local rather than a shared `Mutex` keeps the hot path lock-free and
    /// free of cross-thread contention on the freeze-sensitive callback.
    static PRESSES: RefCell<ButtonPresses> = RefCell::new(ButtonPresses::default());
    /// Function keys whose held action owns an accepted lifecycle. Repeated
    /// key-down events are auto-repeat, not replacement presses; their first
    /// matching key-up ends the lifecycle. A button's repeated press is the
    /// opposite, a lost release, which is why keys stay out of `PRESSES`.
    static HELD_KEYS: RefCell<HashSet<u16>> = RefCell::new(HashSet::new());
}

/// Whether a button event's physical source may be remapped/suppressed.
///
/// macOS fails closed because its hook is global: only a known Logitech,
/// non-trackpad source may be suppressed. Bluetooth-direct Back/Forward
/// gestures are captured through their device-specific HID++ session instead
/// of weakening this policy. Linux and Windows keep an unavailable source
/// eligible. Windows never names one, so a press there reads the selected
/// device's bindings whichever mouse sent it, unless
/// [`DivertedPress::OtherDevice`] rules that device out.
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

/// Off-thread worker for bound actions so the tap callback never injects input
/// or writes a log line. The control is carried only for that log line.
type QueuedAction = (Action, ActionDispatchTarget, PressControl);

fn spawn_action_worker(dispatcher: ActionDispatcher) -> mpsc::SyncSender<QueuedAction> {
    let (tx, rx) = mpsc::sync_channel::<QueuedAction>(64);
    let _ = thread::Builder::new()
        .name("openlogi-action".into())
        .spawn(move || {
            while let Ok((action, target, control)) = rx.recv() {
                log_bound_action(&control, &action);
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
    control: PressControl,
) -> bool {
    if tx.try_send((action, target, control)).is_err() {
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
    capture_safari_pid: impl FnOnce() -> Option<i32>,
) -> EventDisposition {
    // Primary L/R always pass through (suppressing them would brick the mouse).
    if !id.is_os_hook_button() || !button_source_may_remap(device) {
        return EventDisposition::PassThrough;
    }
    if !pressed {
        // Drop the PRESSES borrow before any queueing (re-entrancy freeze hazard).
        let released = PRESSES.with_borrow_mut(|presses| presses.release(id));
        return end_press(id, released, hooks, dispatcher);
    }
    // `try_read` only: a blocking read on the tap thread freezes every pointer
    // event while a config rebuild holds the write lock. Fail open if unavailable.
    let (binding, is_gesture, pointer, diverted) =
        hooks.try_read().map_or((None, false, None, None), |maps| {
            let (binding, is_gesture) = maps.press_binding(id);
            (
                binding,
                is_gesture,
                maps.pointer_press(id),
                maps.diverted_press(id, device),
            )
        });
    let phase = match diverted {
        Some(diverted) => PressPhase::Diverted(diverted),
        None => start_press(
            id,
            binding,
            is_gesture,
            ActionDispatchTarget::for_pointer(pointer, capture_safari_pid),
            dispatcher,
        ),
    };
    let disposition = phase.disposition();
    // A lifecycle the runtime accepted for this press replaces a lost one on
    // the same button. Any other press must end a displaced hold's lifecycle
    // itself, or a shortcut it holds stays down.
    let restarted = matches!(
        phase,
        PressPhase::HeldAsGesture { .. } | PressPhase::Suppressed
    );
    let displaced = PRESSES.with_borrow_mut(|presses| presses.press(id, phase));
    if !restarted && let Some(PressPhase::HeldAsGesture { hold }) = displaced {
        dispatcher.cancel_stale_hook_press(&hold.press);
    }
    disposition
}

/// Offer a press the hook may remap to the runtime, returning the phase it
/// enters.
fn start_press(
    id: ButtonId,
    binding: Option<Binding>,
    is_gesture: bool,
    target: ActionDispatchTarget,
    dispatcher: &ActionDispatcher,
) -> PressPhase {
    if is_gesture {
        // A refused admission — a second gesture button pressed mid-hold —
        // falls through to the single-action path: the first hold wins and
        // this press still means its plain click.
        let now = Instant::now();
        match PRESSES.with_borrow_mut(|presses| presses.admit_gesture(id, now)) {
            HoldAdmission::Refuse => {}
            admission => {
                if let HoldAdmission::Replace(stale) = &admission {
                    dispatcher.cancel_stale_hook_press(stale);
                }
                return dispatcher.try_hook_button_down(id, None, target).map_or(
                    PressPhase::Passthrough,
                    |press| PressPhase::HeldAsGesture {
                        hold: GestureHold::new(press, now),
                    },
                );
            }
        }
    }
    let accepted = binding.is_some_and(|binding| {
        !binding_is_native_click(id, &binding)
            && dispatcher
                .try_hook_button_down(id, Some(&binding), target)
                .is_some()
    });
    if accepted {
        PressPhase::Suppressed
    } else {
        PressPhase::Passthrough
    }
}

/// Finish the release of `id`, whose press was `released`: give it that
/// press's disposition and end the lifecycle the runtime accepted for it.
fn end_press(
    id: ButtonId,
    released: Option<PressPhase>,
    hooks: &SharedHookMaps,
    dispatcher: &ActionDispatcher,
) -> EventDisposition {
    let Some(phase) = released else {
        return EventDisposition::PassThrough;
    };
    let disposition = phase.disposition();
    match phase {
        PressPhase::HeldAsGesture { hold } => {
            let (press, direction) = hold.end();
            if let Some(direction) = direction {
                let action = hooks
                    .try_read()
                    .ok()
                    .map(|m| resolve_gesture_action(&m.gestures, id, direction));
                if let Some(action) = action {
                    dispatcher.try_dispatch_while_pressed(&press, &action);
                }
            }
            dispatcher.try_hook_button_up(id);
        }
        PressPhase::Suppressed => {
            dispatcher.try_hook_button_up(id);
        }
        PressPhase::Diverted(_) | PressPhase::Passthrough => {}
    }
    disposition
}

/// Whether the hook should let the press through natively. A long press
/// always needs the runtime to time it. A wheel-hold binding reaching the hook
/// was not armed over HID++ (no raw-wheel diversion), so it degrades to its
/// click, which stays native when that is the button's own action.
fn binding_is_native_click(id: ButtonId, binding: &Binding) -> bool {
    !matches!(binding, Binding::LongPress(_)) && is_native_click(id, &binding.click_action())
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
    let commit = PRESSES.with_borrow_mut(|presses| presses.accumulate(delta_x, delta_y));
    if let Some((press, button, dir)) = commit {
        let action = hooks
            .try_read()
            .ok()
            .map(|m| resolve_gesture_action(&m.gestures, button, dir));
        if let Some(action) = action {
            debug!(button = %button, ?dir, "gesture swipe committed");
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
    // The hook cannot say which keyboard sent the key, and the selected
    // device is not a stand-in for it.
    if action.needs_firing_device() {
        info!(keycode, action = %action.label(), "key bound to a device-scoped action — passing through");
        return EventDisposition::PassThrough;
    }

    let action_target = capture_target();
    let queued = if action.is_held() {
        let queued = dispatcher.try_hook_key_down(keycode, &action, action_target);
        if queued {
            HELD_KEYS.with_borrow_mut(|keys| {
                keys.insert(keycode);
            });
        }
        queued
    } else {
        try_queue_action(action_tx, action, action_target, PressControl::Key(keycode))
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
    flow: FlowInput,
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

    // The per-hold pointer accumulator lives in the thread-local `PRESSES`;
    // the callback must never block — see the freeze-hazard note in `macos.rs`.
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
                    openlogi_hook::frontmost_safari_pid,
                ),
                MouseEvent::Moved { delta_x, delta_y } => {
                    flow.moved();
                    handle_moved(delta_x, delta_y, &hooks, &dispatcher)
                }
                MouseEvent::CaptureInterrupted => {
                    PRESSES.with_borrow_mut(ButtonPresses::interrupt);
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
                    // No thumb-wheel binding is resolved here. A rebound thumb
                    // wheel is diverted over `0x2150`, so its HID++ capture
                    // session dispatches it and the wheel sends no native
                    // ticks. A native horizontal tick therefore comes from
                    // another source, such as a touchpad pan or RDP input
                    // (#2242, #1708), which the hook cannot attribute.
                    if scroll_source_may_intercept(from_trackpad, device.as_ref()) {
                        return queued_event_disposition(scroll.try_hook_scroll(delta));
                    }
                    EventDisposition::PassThrough
                }
            }
        }
        // Function-key remapper: ordinary actions remain one-shot, while a
        // held chord or held modifiers enter the same down/up/cancel
        // lifecycle as a mouse button. The active set pairs key-up even if
        // modifier state or config changes while the key is down.
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

/// Resolve a gesture, preserving the click/default fallback for sparse maps
/// and honoring an explicit [`Action::None`].
fn resolve_gesture_action(
    gestures: &BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    id: ButtonId,
    direction: GestureDirection,
) -> Action {
    gestures
        .get(&id)
        .and_then(|m| {
            m.get(&direction)
                .or_else(|| m.get(&GestureDirection::Click))
                .cloned()
        })
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
