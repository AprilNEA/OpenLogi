//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    Action, ButtonId, GestureDirection, SwipeAccumulator, default_binding,
};
use openlogi_hid::CaptureChannel;
use openlogi_hook::{EventDisposition, Hook, MouseEvent};
use tracing::{info, warn};

use crate::DpiCycleState;
use crate::event_monitor::SharedEventMonitor;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};

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

/// Tracks which OS-hook button (Middle/Back/Forward) is mid-hold and defers the
/// swipe detection itself to a shared [`SwipeAccumulator`], which commits a swipe
/// *mid-motion* like the HID++ gesture-button path in `openlogi-hid`. This wrapper
/// adds only the button identity the accumulator doesn't track; a press that
/// never commits a direction is a plain click, fired on release.
#[derive(Default)]
struct HoldState {
    button: Option<ButtonId>,
    swipe: SwipeAccumulator,
}

impl HoldState {
    /// Begin a hold for `button`.
    fn begin(&mut self, button: ButtonId) {
        self.button = Some(button);
        self.swipe.begin();
    }

    /// Feed a pointer-move delta into the active hold, tagging a committed swipe
    /// with the held button. Returns `Some((button, direction))` exactly once per
    /// hold, or `None` while still too short, already fired, or not holding.
    fn accumulate(&mut self, dx: i32, dy: i32) -> Option<(ButtonId, GestureDirection)> {
        let button = self.button?;
        self.swipe.accumulate(dx, dy).map(|dir| (button, dir))
    }

    /// End the hold for `button`. Returns `Some(true)` when it ended a hold that
    /// never committed a swipe (the caller should fire the `Click` action),
    /// `Some(false)` when a swipe already fired, and `None` for a stray release
    /// of a button we weren't holding.
    fn end(&mut self, button: ButtonId) -> Option<bool> {
        if self.button == Some(button) {
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
}

thread_local! {
    /// In-progress gesture hold, one instance per hook-callback thread: the
    /// single macOS tap thread, or — on Linux — one thread per device, so two
    /// mice never share a hold (a press on one can't hijack the other's swipe).
    /// Thread-local rather than a shared `Mutex` keeps the hot path lock-free and
    /// free of cross-thread contention on the freeze-sensitive callback.
    static HOLD: RefCell<HoldState> = RefCell::new(HoldState::default());
}

/// How long a repeatable action's button must stay down before the action
/// starts re-firing. Mirrors a keyboard's typematic delay: long enough that an
/// ordinary click never repeats by accident.
const REPEAT_INITIAL_DELAY: Duration = Duration::from_millis(400);

/// The gap before the second fire. Deliberately slow: a short hold should nudge
/// the value a step or two, not overshoot it.
const REPEAT_START_INTERVAL: Duration = Duration::from_millis(220);

/// The floor the gap ramps down to. Fast enough to cross a volume range in one
/// hold, slow enough that the user can still stop on a value.
const REPEAT_MIN_INTERVAL: Duration = Duration::from_millis(45);

/// The gap shrinks by this fraction (`17/20` = 0.85) after every fire, so a hold
/// accelerates instead of running at one flat rate. Integer maths keeps the ramp
/// exactly reproducible in tests.
const REPEAT_RAMP_NUM: u64 = 17;
/// Denominator of [`REPEAT_RAMP_NUM`].
const REPEAT_RAMP_DEN: u64 = 20;

/// The gap to use after the one that just elapsed: 15% shorter, clamped at
/// [`REPEAT_MIN_INTERVAL`].
///
/// From [`REPEAT_START_INTERVAL`] this reaches the floor after ~11 fires, about
/// 1.2 s into the hold — gentle enough to land on a single step, quick enough
/// that a long hold sweeps the whole range.
fn ramp(current: Duration) -> Duration {
    let next = Duration::from_millis(
        u64::try_from(current.as_millis()).unwrap_or(u64::MAX) * REPEAT_RAMP_NUM / REPEAT_RAMP_DEN,
    );
    next.max(REPEAT_MIN_INTERVAL)
}

/// What a capture path tells the repeat worker.
///
/// Both input paths use this: the OS hook (for buttons it remaps directly) and
/// the HID++ capture session (for buttons diverted to get the release edge).
///
/// Every variant names the button it concerns. Two thumb buttons can be held at
/// once, so an unkeyed stop would let releasing one end the other's cycle while
/// it is still physically down.
pub enum RepeatCmd {
    /// A repeatable action's button went down — begin the delay-then-repeat
    /// cycle for that button, superseding its own previous cycle only.
    Start(ButtonId, Action),
    /// The button came up — stop re-firing that button.
    Stop(ButtonId),
    /// Capture was interrupted, so no release edge is coming for anything still
    /// held: end every cycle rather than leave an action firing with nothing left
    /// to stop it.
    StopAll,
}

/// One button's in-flight repeat cycle.
struct RepeatCycle {
    /// The action to re-fire, resolved when the press arrived.
    action: Action,
    /// When this button's next re-fire is due.
    due: Instant,
    /// The gap to apply *after* the fire that is currently pending.
    interval: Duration,
}

/// Every hold currently repeating, keyed by button.
///
/// Split out from the worker thread so the scheduling is testable without
/// dispatching real actions: the worker only sleeps and dispatches, this decides
/// what is due and when to wake.
#[derive(Default)]
struct RepeatCycles {
    cycles: BTreeMap<ButtonId, RepeatCycle>,
}

impl RepeatCycles {
    /// Begin (or restart) one button's cycle. A restart resets the delay *and*
    /// the ramp, so every hold starts slow again.
    fn start(&mut self, button: ButtonId, action: Action, now: Instant) {
        self.cycles.insert(
            button,
            RepeatCycle {
                action,
                due: now + REPEAT_INITIAL_DELAY,
                interval: REPEAT_START_INTERVAL,
            },
        );
    }

    /// End one button's cycle, leaving any other held button running. A stop for
    /// a button that is not repeating is a no-op, which is what lets the callers
    /// send it unconditionally on release.
    fn stop(&mut self, button: ButtonId) {
        self.cycles.remove(&button);
    }

    /// End every cycle — see [`RepeatCmd::StopAll`].
    fn stop_all(&mut self) {
        self.cycles.clear();
    }

    /// How long the worker may sleep before the soonest re-fire, or `None` when
    /// nothing is held (park until a press arrives).
    fn next_wait(&self, now: Instant) -> Option<Duration> {
        self.cycles
            .values()
            .map(|cycle| cycle.due)
            .min()
            .map(|due| due.saturating_duration_since(now))
    }

    /// Take every action whose gap has elapsed, advancing each ramp. Buttons ramp
    /// independently: one held longer is already firing faster.
    fn take_due(&mut self, now: Instant) -> Vec<Action> {
        let mut due = Vec::new();
        for cycle in self.cycles.values_mut().filter(|cycle| cycle.due <= now) {
            due.push(cycle.action.clone());
            cycle.due = now + cycle.interval;
            cycle.interval = ramp(cycle.interval);
        }
        due
    }
}

/// Spawns the worker that re-fires a held button's action, and returns the
/// sender the hook callback signals it with.
///
/// The delay and the interval are slept here, never in the callback: the
/// callback must not block (see the freeze-hazard note in `macos.rs`). The
/// worker also keeps the repeat state off the thread-local [`HOLD`], so a
/// synthesized event re-entering the tap cannot double-borrow it.
///
/// [`Sender`] is `Sync` as of Rust 1.72, so the callback can hold it directly
/// without a mutex.
pub fn spawn_repeater(
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture: CaptureChannel,
) -> Sender<RepeatCmd> {
    let (tx, rx) = mpsc::channel::<RepeatCmd>();
    std::thread::spawn(move || {
        let mut cycles = RepeatCycles::default();
        loop {
            // Park until a press arrives when nothing is held; otherwise wake on
            // whichever comes first, the next command or the soonest re-fire.
            let cmd = match cycles.next_wait(Instant::now()) {
                Some(wait) => match rx.recv_timeout(wait) {
                    Ok(cmd) => Some(cmd),
                    Err(RecvTimeoutError::Timeout) => None,
                    // The agent is shutting down.
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match rx.recv() {
                    Ok(cmd) => Some(cmd),
                    Err(_) => return,
                },
            };
            match cmd {
                Some(RepeatCmd::Start(button, action)) => {
                    cycles.start(button, action, Instant::now());
                }
                Some(RepeatCmd::Stop(button)) => cycles.stop(button),
                Some(RepeatCmd::StopAll) => cycles.stop_all(),
                // A gap elapsed: fire everything now due. The first press was
                // already dispatched by the caller, so this is strictly the 2nd
                // fire onward.
                None => {
                    for action in cycles.take_due(Instant::now()) {
                        dispatch_action(&action, &dpi_cycle, &capture);
                    }
                }
            }
        }
    });
    tx
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    hooks: SharedHookMaps,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture: CaptureChannel,
    monitor: SharedEventMonitor,
) -> Option<Hook> {
    if !Hook::has_accessibility() {
        warn!(
            "Accessibility not granted — events will not be captured. \
             Open System Settings → Privacy & Security → Accessibility."
        );
        return None;
    }

    // Hold-to-repeat runs on its own thread so the callback never sleeps.
    let repeat = spawn_repeater(Arc::clone(&dpi_cycle), Arc::clone(&capture));

    // The per-hold pointer accumulator lives in the thread-local `HOLD`; the
    // callback must never block — see the freeze-hazard note in `macos.rs`.
    let result = Hook::start(move |event| {
        // Mirror the raw event to the GUI's live monitor first (a single relaxed
        // atomic load while monitoring is off — see `event_monitor`), before any
        // remapping decides its disposition.
        monitor.record(&event);
        match event {
            MouseEvent::Button { id, pressed } => {
                // The CGEventTap only sees standard buttons 0-4. We remap
                // Middle/Back/Forward; the primary L/R clicks always pass through
                // (suppressing them would brick the mouse), and the DPI / thumb /
                // dedicated gesture button aren't visible to the tap at all — the
                // dedicated gesture button is captured separately over HID++.
                if !id.is_os_hook_button() {
                    return EventDisposition::PassThrough;
                }

                // Gesture button: suppress the native click and begin a hold. The
                // swipe commits mid-motion in the `Moved` arm; here, on release, we
                // only fire the plain `Click` when no swipe committed. The cursor is
                // free to drift via the pass-through `Moved` events during the hold.
                if pressed {
                    let is_gesture = hooks.read().is_ok_and(|m| m.gestures.contains_key(&id));
                    if is_gesture {
                        HOLD.with_borrow_mut(|h| h.begin(id));
                        return EventDisposition::Suppress;
                    }
                } else {
                    // Release: end the hold and release the `HOLD` borrow *before* any
                    // dispatch — the callback must stay lock-light, since a
                    // synthesized event could otherwise re-enter the tap and re-borrow
                    // `HOLD` (a RefCell double-borrow panic, freeze hazard).
                    let ended = HOLD.with_borrow_mut(|h| h.end(id));
                    if let Some(was_click) = ended {
                        if was_click {
                            // No swipe committed → fire the plain click. Resolve to an
                            // owned action (so no lock is held across dispatch), then
                            // dispatch with the guard already dropped.
                            let action = hooks
                                .read()
                                .ok()
                                .map(|m| resolve_gesture_click(&m.gestures, id));
                            if let Some(action) = action {
                                info!(button = %id, action = %action.label(), "gesture click → executing bound action");
                                dispatch_action(&action, &dpi_cycle, &capture);
                            }
                        }
                        return EventDisposition::Suppress;
                    }
                }

                // Single-action button.
                let action = hooks.read().ok().and_then(|m| m.bindings.get(&id).cloned());
                let Some(action) = action else {
                    // Unbound → leave the physical button to the OS.
                    return EventDisposition::PassThrough;
                };

                // A button left on its own native click (e.g. Middle → MiddleClick)
                // should just do that click; suppressing and re-synthesising it
                // would be pointless churn.
                if is_native_click(id, &action) {
                    return EventDisposition::PassThrough;
                }

                if pressed {
                    info!(button = %id, action = %action.label(), "button → executing bound action");
                    dispatch_action(&action, &dpi_cycle, &capture);
                    // Increment-style actions keep firing while the button is
                    // held; the worker owns the timing. A send failure just
                    // means the worker is gone, which costs only the repeat.
                    if action.is_repeatable() {
                        let _ = repeat.send(RepeatCmd::Start(id, action));
                    }
                } else {
                    // Unconditional, but keyed to this button: a stop for a
                    // button that is not repeating is a no-op, so a binding that
                    // changed mid-hold still ends the cycle its own press
                    // started — without touching another button's hold.
                    let _ = repeat.send(RepeatCmd::Stop(id));
                }
                EventDisposition::Suppress
            }
            MouseEvent::Moved { delta_x, delta_y } => {
                // Feed an in-progress hold; a committed swipe fires here, mid-motion.
                // Always pass through so the cursor keeps moving — the swipe is read,
                // not consumed (the B2 cursor-drift tradeoff vs. a HID++ raw-XY divert
                // that would freeze the pointer).
                let commit = HOLD.with_borrow_mut(|h| h.accumulate(delta_x, delta_y));
                if let Some((button, dir)) = commit {
                    // Resolve to an owned action and drop the read guard before
                    // dispatch (same lock-light rule as the release arm). The button
                    // can leave the gesture set mid-hold (a per-app rebuild); the
                    // commit has already armed `fired`, so the release won't fire a
                    // click. Fall back to the same click action the release path uses
                    // so the suppressed press is never swallowed into nothing —
                    // symmetric with `resolve_gesture_click`.
                    let action = hooks.read().ok().map(|m| {
                        m.gestures
                            .get(&button)
                            .and_then(|dirs| dirs.get(&dir).cloned())
                            .unwrap_or_else(|| resolve_gesture_click(&m.gestures, button))
                    });
                    if let Some(action) = action {
                        info!(button = %button, ?dir, action = %action.label(), "gesture swipe → executing bound action");
                        dispatch_action(&action, &dpi_cycle, &capture);
                    }
                }
                EventDisposition::PassThrough
            }
            MouseEvent::CaptureInterrupted => {
                // The OS dropped events (tap disabled); cancel any hold so a lost
                // button-up can't later commit a phantom swipe off ordinary motion.
                HOLD.with_borrow_mut(HoldState::cancel);
                // Same reasoning for the repeat cycles: without this, a swallowed
                // button-up would leave the action firing forever.
                let _ = repeat.send(RepeatCmd::StopAll);
                EventDisposition::PassThrough
            }
            MouseEvent::Scroll { .. } => EventDisposition::PassThrough,
        }
    });

    match result {
        Ok(hook) => {
            info!("OS mouse hook installed");
            Some(hook)
        }
        Err(e) => {
            warn!(error = %e, "could not install OS mouse hook — events will not be captured");
            None
        }
    }
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

/// Route a bound action either to OS-level event synthesis
/// ([`Action::execute`]) or to one of OpenLogi's hardware-side handlers.
///
/// `dpi_cycle` is held across a write lock long enough to advance the index
/// and snapshot the new DPI + target; the actual HID write spawns its own
/// thread via [`write_dpi_in_background`] to keep event callbacks non-blocking.
/// `capture` lets those writes reuse the capture session's open channel.
pub fn dispatch_action(
    action: &Action,
    dpi_cycle: &Arc<RwLock<DpiCycleState>>,
    capture: &CaptureChannel,
) {
    let next = match action {
        Action::CycleDpiPresets => match dpi_cycle.write() {
            Ok(mut guard) => guard.cycle(),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                None
            }
        },
        Action::SetDpiPreset(i) => match dpi_cycle.write() {
            Ok(mut guard) => guard.set(usize::from(*i)),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — set skipped");
                None
            }
        },
        Action::ToggleSmartShift => {
            let target = dpi_cycle.read().ok().and_then(|g| g.target.clone());
            info!("SmartShift toggle → flipping wheel mode");
            toggle_smartshift_in_background(Some(capture), target);
            return;
        }
        other => {
            openlogi_inject::execute(other);
            None
        }
    };
    if let Some((dpi, target)) = next {
        info!(dpi, "DPI action → writing to device");
        write_dpi_in_background(Some(capture), target, dpi);
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

    /// The gap between fires is long enough that a test can step time by hand:
    /// jump past the initial delay, then past one interval, without sleeping.
    fn past(now: Instant, gap: Duration) -> Instant {
        now + gap + Duration::from_millis(1)
    }

    #[test]
    fn a_release_stops_only_the_button_that_was_released() {
        // Two thumb buttons held at once is the case an unkeyed stop got wrong:
        // letting go of one killed the other's repeat while it was still down.
        let start = Instant::now();
        let mut cycles = RepeatCycles::default();
        cycles.start(ButtonId::Back, Action::VolumeDown, start);
        cycles.start(ButtonId::Forward, Action::VolumeUp, start);
        cycles.stop(ButtonId::Back);

        let due = cycles.take_due(past(start, REPEAT_INITIAL_DELAY));
        assert_eq!(
            due,
            vec![Action::VolumeUp],
            "the button still held must keep repeating on its own"
        );
    }

    #[test]
    fn an_unrelated_buttons_release_leaves_the_hold_running() {
        // The OS hook sends a keyed stop on *every* bound button's release, so a
        // click on some other button must not disturb a hold in progress.
        let start = Instant::now();
        let mut cycles = RepeatCycles::default();
        cycles.start(ButtonId::Back, Action::VolumeDown, start);
        cycles.stop(ButtonId::MiddleClick);

        assert_eq!(
            cycles.take_due(past(start, REPEAT_INITIAL_DELAY)),
            vec![Action::VolumeDown]
        );
    }

    #[test]
    fn stop_all_ends_every_hold() {
        // What a capture session sends as it goes away: no release edge is
        // coming for anything still held, so nothing may stay armed.
        let start = Instant::now();
        let mut cycles = RepeatCycles::default();
        cycles.start(ButtonId::Back, Action::VolumeDown, start);
        cycles.start(ButtonId::Forward, Action::VolumeUp, start);
        cycles.stop_all();

        assert!(
            cycles
                .take_due(past(start, REPEAT_INITIAL_DELAY))
                .is_empty(),
            "an interrupted capture must not leave an action firing"
        );
        assert_eq!(
            cycles.next_wait(start),
            None,
            "with nothing held the worker parks instead of spinning"
        );
    }

    #[test]
    fn nothing_fires_before_the_initial_delay_elapses() {
        let start = Instant::now();
        let mut cycles = RepeatCycles::default();
        cycles.start(ButtonId::Back, Action::VolumeDown, start);

        // 90% of the way into the delay — a firmly ordinary click.
        let just_short = REPEAT_INITIAL_DELAY.mul_f32(0.9);
        assert!(
            cycles.take_due(start + just_short).is_empty(),
            "an ordinary click must never repeat"
        );
        assert_eq!(
            cycles.next_wait(start),
            Some(REPEAT_INITIAL_DELAY),
            "the worker sleeps exactly until the first re-fire is due"
        );
    }

    #[test]
    fn each_button_ramps_on_its_own_clock() {
        // Independent ramps: the button held longer is already firing faster, and
        // one button's fire must not reschedule the other's.
        let start = Instant::now();
        let mut cycles = RepeatCycles::default();
        cycles.start(ButtonId::Back, Action::VolumeDown, start);

        // Back has been repeating for a while before Forward is even pressed.
        let mut now = past(start, REPEAT_INITIAL_DELAY);
        for _ in 0..6 {
            assert_eq!(cycles.take_due(now), vec![Action::VolumeDown]);
            now = past(now, REPEAT_START_INTERVAL);
        }
        cycles.start(ButtonId::Forward, Action::VolumeUp, now);

        // Forward serves out its own full initial delay, during which Back keeps
        // firing alone.
        assert_eq!(cycles.take_due(now), vec![Action::VolumeDown]);
        assert_eq!(
            cycles.take_due(past(now, REPEAT_INITIAL_DELAY)).len(),
            2,
            "once its delay is up, both buttons fire"
        );
    }

    #[test]
    fn a_re_press_restarts_that_buttons_delay_and_ramp() {
        let start = Instant::now();
        let mut cycles = RepeatCycles::default();
        cycles.start(ButtonId::Back, Action::VolumeDown, start);
        let mut now = past(start, REPEAT_INITIAL_DELAY);
        assert_eq!(cycles.take_due(now), vec![Action::VolumeDown]);

        // A fresh press (binding swap, or a re-press that outran the release).
        now += Duration::from_millis(10);
        cycles.start(ButtonId::Back, Action::VolumeUp, now);
        assert!(
            cycles.take_due(now + REPEAT_START_INTERVAL).is_empty(),
            "the delay starts over, so the hold ramps up slowly again"
        );
        assert_eq!(
            cycles.take_due(past(now, REPEAT_INITIAL_DELAY)),
            vec![Action::VolumeUp],
            "and it fires the newly bound action"
        );
    }

    #[test]
    fn the_repeat_ramp_accelerates_and_then_holds_at_the_floor() {
        let mut interval = REPEAT_START_INTERVAL;
        for _ in 0..40 {
            let next = ramp(interval);
            assert!(
                next <= interval,
                "the gap must never grow: {interval:?} → {next:?}"
            );
            assert!(next >= REPEAT_MIN_INTERVAL, "the floor must hold");
            interval = next;
        }
        assert_eq!(
            interval, REPEAT_MIN_INTERVAL,
            "a long hold settles at the fastest rate"
        );
    }

    #[test]
    fn the_repeat_ramp_reaches_the_floor_in_about_a_second() {
        // Feel check: too quick and a short hold overshoots, too slow and a long
        // hold never gets anywhere. Sum the gaps until the rate stops changing.
        let mut interval = REPEAT_START_INTERVAL;
        let mut elapsed = Duration::ZERO;
        let mut fires = 0;
        while interval > REPEAT_MIN_INTERVAL {
            elapsed += interval;
            interval = ramp(interval);
            fires += 1;
        }
        assert!(
            (8..=16).contains(&fires),
            "expected ~11 fires to reach the floor, got {fires}"
        );
        assert!(
            elapsed >= Duration::from_millis(900) && elapsed <= Duration::from_millis(1600),
            "expected ~1.2 s of holding to reach the floor, got {elapsed:?}"
        );
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
