//! Testable state and timing for the macOS HID tap safety watchdogs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(super) const CALLBACK_STUCK_BUDGET: Duration = Duration::from_millis(200);
/// How often the callback watchdog polls.
pub(super) const CALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// The callback watchdog's [`OBSERVATION_GAP`]: the most stall one poll may
/// charge for a gap that spanned a kernel sleep or wake. The freeze that
/// leaves the lifecycle watchdog unscheduled leaves this thread unscheduled
/// too, and a callback that was entered when it began is not stuck for having
/// been frozen with the rest of the process. Every other gap is charged in
/// full.
pub(super) const CALLBACK_OBSERVATION_GAP: Duration = CALLBACK_POLL_INTERVAL.saturating_mul(5);
pub(super) const TAP_SHUTDOWN_BUDGET: Duration = Duration::from_millis(1_500);
/// How long the tap thread may stay inside one between-slice capability probe
/// before the watchdog treats it as wedged.
///
/// [`TapPhase::Probing`] time is not the freeze hazard [`TAP_SHUTDOWN_BUDGET`]
/// guards: the thread is inside CoreGraphics/TCC calls that are slow rather
/// than stuck, and an active tap whose thread is not servicing its run loop is
/// already bounded by CoreGraphics' own tap timeout, which disables the tap and
/// lets events through. Only a probe that never returns is hazardous, so this
/// budget is generous enough to clear the seconds WindowServer can go without
/// answering around a sleep transition (#952) and still bounded.
pub(super) const TAP_PROBE_BUDGET: Duration = Duration::from_secs(10);
/// How often the lifecycle watchdog evaluates.
pub(super) const LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// The most stall one evaluation may charge for a gap that spanned a kernel
/// sleep or wake ([`PowerEpoch`]). Around such a transition the watchdog
/// thread has fired 1.0 s and 4.8 s past its budget (#952), which a thread
/// evaluating every poll cannot do: it was not running, and neither was the
/// tap thread it judges, so the excess is no evidence against it. A gap with
/// no transition in it is charged in full — a watchdog merely delayed by
/// scheduling still catches a wedged tap on schedule — and the cap still
/// charges every discounted gap, so repeated sleep cycles cannot hide a
/// wedged tap thread indefinitely.
pub(super) const OBSERVATION_GAP: Duration = LIFECYCLE_POLL_INTERVAL.saturating_mul(5);
/// How many re-arms the hook grants inside [`REARM_WINDOW`] before it gives
/// the tap up.
pub(super) const REARM_LIMIT: u32 = 10;
/// The rolling window [`REARM_LIMIT`] applies to. Re-arming happens at most
/// once per run-loop slice, so a spent budget means the OS has been disabling
/// the tap for seconds on end.
pub(super) const REARM_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(super) enum TapPhase {
    /// The tap thread has not begun CoreGraphics tap creation, so no tap can exist.
    Starting,
    /// The tap thread is creating or activating a tap that may already exist.
    Arming,
    Armed,
    /// The tap is live and its thread is between run-loop slices, inside the
    /// CoreGraphics and TCC calls that re-check the Accessibility grant and
    /// re-arm the tap. Those are WindowServer round trips, not the tap's own
    /// event servicing, so they are budgeted by [`TAP_PROBE_BUDGET`].
    Probing,
    TapStopped,
    ThreadExited,
}

impl TapPhase {
    const fn decode(value: u8) -> Self {
        if value > Self::ThreadExited as u8 {
            // An unknown byte cannot prove that the HID tap was destroyed.
            // Treat it as hazardous so both watchdogs remain armed and the
            // lifecycle timeout can fail safe instead of panicking or disarming.
            return Self::Armed;
        }
        match value {
            0 => Self::Starting,
            1 => Self::Arming,
            2 => Self::Armed,
            3 => Self::Probing,
            4 => Self::TapStopped,
            _ => Self::ThreadExited,
        }
    }
}

/// Whether the tap callback is idle or the monotonic millisecond when it entered.
///
/// Zero is idle; [`WatchdogSignals::now_millis`] reserves nonzero values for
/// entries. One Release store publishes each whole state, so an Acquire load
/// cannot observe "entered" without its matching timestamp as it could with
/// separate flag and timestamp atomics.
#[derive(Debug, Default)]
pub(super) struct CallbackActivity(AtomicU64);

impl CallbackActivity {
    pub fn enter(&self, entered_at_ms: u64) {
        debug_assert_ne!(entered_at_ms, 0);
        self.0.store(entered_at_ms, Ordering::Release);
    }

    pub fn exit(&self) {
        self.0.store(0, Ordering::Release);
    }

    pub fn entered_at_ms(&self) -> Option<u64> {
        match self.0.load(Ordering::Acquire) {
            0 => None,
            entered_at_ms => Some(entered_at_ms),
        }
    }
}

/// Atomics shared by the tap, stopper, and watchdog threads.
///
/// A stop request is a separate latch from `phase`: it must never imply that
/// the active HID tap has actually been detached.
#[derive(Debug)]
pub(super) struct WatchdogSignals {
    // `Instant` uses CLOCK_UPTIME_RAW on macOS: monotonic, and paused while
    // the system sleeps. The transition around sleep is not paused, and the
    // watchdog thread has been observed not running for seconds of it — which
    // is why the lifecycle watchdog discounts a gap in its own schedule that
    // spanned such a transition.
    origin: Instant,
    phase: AtomicU8,
    stop_requested: AtomicBool,
    tap_progress_at_ms: AtomicU64,
}

impl Default for WatchdogSignals {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            phase: AtomicU8::new(TapPhase::Starting as u8),
            stop_requested: AtomicBool::new(false),
            tap_progress_at_ms: AtomicU64::new(0),
        }
    }
}

impl WatchdogSignals {
    pub fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    pub fn now_millis(&self) -> u64 {
        u64::try_from(self.now().as_millis())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }

    pub fn phase(&self) -> TapPhase {
        TapPhase::decode(self.phase.load(Ordering::Acquire))
    }

    pub fn set_phase(&self, phase: TapPhase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    /// Publish fresh progress before the short watchdog budget applies.
    pub fn mark_armed(&self) {
        // The watchdog reads phase before progress. Publishing `Armed` first
        // could charge a completed probe against its old progress mark.
        self.mark_tap_progress();
        self.set_phase(TapPhase::Armed);
    }

    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    pub fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    pub fn mark_tap_progress(&self) {
        self.tap_progress_at_ms
            .store(self.now_millis(), Ordering::Release);
    }

    pub fn tap_progress_at(&self) -> Duration {
        Duration::from_millis(
            self.tap_progress_at_ms
                .load(Ordering::Acquire)
                .saturating_sub(1),
        )
    }

    pub fn thread_exit_guard(self: &Arc<Self>) -> TapThreadExitGuard {
        TapThreadExitGuard(Arc::clone(self))
    }
}

pub(super) struct TapThreadExitGuard(Arc<WatchdogSignals>);

impl Drop for TapThreadExitGuard {
    fn drop(&mut self) {
        self.0.set_phase(TapPhase::ThreadExited);
    }
}

/// The kernel's last sleep and wake instants — `kern.sleeptime` and
/// `kern.waketime`, as microseconds since the epoch — compared for equality
/// only. A change between two evaluations means the gap contained a system
/// sleep or wake, the one kind of gap the watchdogs discount: both late #952
/// exits on the reporting host straddled one (5 ms after `System Wake`, and
/// within the second of a DarkWake). `kern.waketime` moves on every wake from
/// sleep, dark or full, not on a DarkWake's promotion to a full wake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PowerEpoch {
    pub slept_us: i64,
    pub woke_us: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LifecycleObservation {
    pub phase: TapPhase,
    pub stop_requested: bool,
    pub tap_progress_at: Duration,
    pub power: PowerEpoch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LifecycleExitReason {
    TapThreadStalled,
    StopTimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LifecycleDecision {
    Continue,
    Complete,
    Exit {
        reason: LifecycleExitReason,
        /// Stall the watchdog was awake to see; what the budget is judged on.
        watched: Duration,
        /// Uptime since the stall began. The excess over `watched` is time the
        /// watchdog itself was not running.
        stalled: Duration,
    },
}

#[derive(Debug, Default)]
pub(super) struct LifecycleWatchdog {
    stop_at: Option<Duration>,
    /// When the previous evaluation ran, and the power epoch it saw.
    last_evaluated: Option<(Duration, PowerEpoch)>,
    /// The stall being timed, if any.
    stall: Option<Stall>,
}

/// One stall under watch: what it would exit for, when it began, which budget
/// judges it, and how much of it the watchdog was awake to see.
#[derive(Clone, Copy, Debug)]
struct Stall {
    reason: LifecycleExitReason,
    started: Duration,
    /// A budget change is a new stall: a stop that waited out a slow probe
    /// under [`TAP_PROBE_BUDGET`] gets [`TAP_SHUTDOWN_BUDGET`] for the
    /// teardown that follows, instead of being exited the moment the probe
    /// returns because the probe alone outlasted the short budget.
    budget: Duration,
    watched: Duration,
}

impl LifecycleWatchdog {
    pub fn evaluate(
        &mut self,
        now: Duration,
        observation: LifecycleObservation,
    ) -> LifecycleDecision {
        // The whole gap counts, unless a kernel sleep or wake fell inside it:
        // then the process was frozen for an unknown part of it, and at most
        // `OBSERVATION_GAP` is charged.
        let credit = self.last_evaluated.map_or(Duration::ZERO, |(last, power)| {
            let gap = now.saturating_sub(last);
            if power == observation.power {
                gap
            } else {
                gap.min(OBSERVATION_GAP)
            }
        });
        self.last_evaluated = Some((now, observation.power));

        match observation.phase {
            TapPhase::Starting => return LifecycleDecision::Continue,
            TapPhase::ThreadExited => return LifecycleDecision::Complete,
            TapPhase::Arming | TapPhase::Armed | TapPhase::Probing | TapPhase::TapStopped => {}
        }

        if observation.stop_requested {
            self.stop_at.get_or_insert(now);
        }
        if observation.phase == TapPhase::TapStopped && !observation.stop_requested {
            return LifecycleDecision::Complete;
        }

        let timeout = if let Some(stopped) = self.stop_at {
            Some((LifecycleExitReason::StopTimedOut, stopped))
        } else if matches!(
            observation.phase,
            TapPhase::Arming | TapPhase::Armed | TapPhase::Probing
        ) {
            Some((
                LifecycleExitReason::TapThreadStalled,
                observation.tap_progress_at,
            ))
        } else {
            None
        };
        let Some((reason, started)) = timeout else {
            self.stall = None;
            return LifecycleDecision::Continue;
        };
        // A thread that published `Probing` reached that store, so it is alive
        // and inside a named CoreGraphics/TCC call rather than wedged servicing
        // the tap. Judge it against the probe budget for either exit reason —
        // a stop request landing on a slow probe is the same situation.
        let budget = if observation.phase == TapPhase::Probing {
            TAP_PROBE_BUDGET
        } else {
            TAP_SHUTDOWN_BUDGET
        };
        // A stall seen for the first time is charged at most one credit for
        // whatever preceded this evaluation.
        let watched = match self.stall {
            Some(stall)
                if stall.reason == reason && stall.started == started && stall.budget == budget =>
            {
                stall.watched + credit
            }
            _ => now.saturating_sub(started).min(credit),
        };
        self.stall = Some(Stall {
            reason,
            started,
            budget,
            watched,
        });
        if watched >= budget {
            LifecycleDecision::Exit {
                reason,
                watched,
                stalled: now.saturating_sub(started),
            }
        } else {
            LifecycleDecision::Continue
        }
    }
}

/// Bounded re-arming of a tap the OS has disabled.
///
/// `TapDisabledByUserInput` fires during ordinary heavy input and self-heals,
/// so a burst has to be re-armed or the hook goes deaf. A tap the OS disables
/// again slice after slice is a different animal: nothing is servicing it, and
/// re-enabling it keeps an active HID tap gating events it will never answer
/// for. Give the burst room, then stop fighting and let the tap go.
#[derive(Debug, Default)]
pub(super) struct RearmBudget {
    window_start: Option<Duration>,
    used: u32,
}

impl RearmBudget {
    /// Charge a re-arm at `now`; `false` once this window's budget is spent.
    pub fn allow(&mut self, now: Duration) -> bool {
        match self.window_start {
            Some(start) if now.saturating_sub(start) < REARM_WINDOW => self.used += 1,
            _ => {
                self.window_start = Some(now);
                self.used = 1;
            }
        }
        self.used <= REARM_LIMIT
    }
}

/// A callback entry the watchdog was awake to see outlast [`CALLBACK_STUCK_BUDGET`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StuckCallback {
    /// Stall the watchdog was awake to see; what the budget is judged on.
    pub watched: Duration,
    /// Uptime since the callback was entered.
    pub stalled: Duration,
}

/// The callback watchdog's decision state: the same watched-time rule as
/// [`LifecycleWatchdog`], over the callback's entry time instead of the tap
/// thread's progress mark.
#[derive(Debug, Default)]
pub(super) struct CallbackWatchdog {
    /// When the previous poll ran, and the power epoch it saw.
    last_polled: Option<(u64, PowerEpoch)>,
    /// The entry being timed and how much of it the watchdog was awake to see.
    entry: Option<(u64, Duration)>,
}

impl CallbackWatchdog {
    /// A watchdog that began watching at `now_ms`. Its thread sleeps one poll
    /// interval before the first poll, so that poll's gap — and any
    /// scheduling delay in it — is charged like every later one, instead of
    /// a first poll with no predecessor crediting nothing.
    pub fn watching_since(now_ms: u64, power: PowerEpoch) -> Self {
        Self {
            last_polled: Some((now_ms, power)),
            entry: None,
        }
    }

    /// Fold one poll at `now_ms`; `entered_at_ms` is the callback's entry time
    /// while it is inside the callback, `None` while it is idle.
    pub fn evaluate(
        &mut self,
        now_ms: u64,
        entered_at_ms: Option<u64>,
        power: PowerEpoch,
    ) -> Option<StuckCallback> {
        let credit = self.last_polled.map_or(Duration::ZERO, |(last, seen)| {
            let gap = Duration::from_millis(now_ms.saturating_sub(last));
            if seen == power {
                gap
            } else {
                gap.min(CALLBACK_OBSERVATION_GAP)
            }
        });
        self.last_polled = Some((now_ms, power));
        let Some(entered_at_ms) = entered_at_ms else {
            self.entry = None;
            return None;
        };
        let stalled = Duration::from_millis(now_ms.saturating_sub(entered_at_ms));
        // An entry seen for the first time is charged at most one credit: a
        // fresh high-frequency event must not inherit an older entry's stall.
        let watched = match self.entry {
            Some((entered, watched)) if entered == entered_at_ms => watched + credit,
            _ => stalled.min(credit),
        };
        self.entry = Some((entered_at_ms, watched));
        (watched >= CALLBACK_STUCK_BUDGET).then_some(StuckCallback { watched, stalled })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_phase_decoder_is_total_and_fails_safe() {
        assert_eq!(TapPhase::decode(0), TapPhase::Starting);
        assert_eq!(TapPhase::decode(1), TapPhase::Arming);
        assert_eq!(TapPhase::decode(2), TapPhase::Armed);
        assert_eq!(TapPhase::decode(3), TapPhase::Probing);
        assert_eq!(TapPhase::decode(4), TapPhase::TapStopped);
        assert_eq!(TapPhase::decode(5), TapPhase::ThreadExited);
        assert_eq!(TapPhase::decode(6), TapPhase::Armed);
        assert_eq!(TapPhase::decode(u8::MAX), TapPhase::Armed);

        let signals = WatchdogSignals::default();
        signals.phase.store(u8::MAX, Ordering::Relaxed);
        assert_eq!(signals.phase(), TapPhase::Armed);
    }

    #[test]
    fn callback_activity_publishes_one_complete_state() {
        let activity = CallbackActivity::default();
        assert_eq!(activity.entered_at_ms(), None);

        activity.enter(42);
        assert_eq!(activity.entered_at_ms(), Some(42));

        activity.enter(43);
        assert_eq!(activity.entered_at_ms(), Some(43));

        activity.exit();
        assert_eq!(activity.entered_at_ms(), None);
    }

    fn observation(
        phase: TapPhase,
        stop_requested: bool,
        tap_progress_at: Duration,
    ) -> LifecycleObservation {
        LifecycleObservation {
            phase,
            stop_requested,
            tap_progress_at,
            power: PowerEpoch::default(),
        }
    }

    /// `observation` as seen after the kernel slept and woke once more.
    fn after_sleep(observation: LifecycleObservation) -> LifecycleObservation {
        LifecycleObservation {
            power: PowerEpoch {
                slept_us: observation.power.slept_us + 1,
                woke_us: observation.power.woke_us + 1,
            },
            ..observation
        }
    }

    /// Drive `watchdog` the way its thread does: one evaluation per poll tick
    /// after the previous one, each of which must keep watching, then one at
    /// `until`, whose decision is returned.
    fn watched(
        watchdog: &mut LifecycleWatchdog,
        until: Duration,
        observation: LifecycleObservation,
    ) -> LifecycleDecision {
        let mut tick = watchdog
            .last_evaluated
            .map_or(Duration::ZERO, |(last, _)| last + LIFECYCLE_POLL_INTERVAL);
        while tick < until {
            assert_eq!(
                watchdog.evaluate(tick, observation),
                LifecycleDecision::Continue,
                "no decision before {until:?} (tick {tick:?})"
            );
            tick += LIFECYCLE_POLL_INTERVAL;
        }
        watchdog.evaluate(until, observation)
    }

    fn exit(
        reason: LifecycleExitReason,
        watched: Duration,
        stalled: Duration,
    ) -> LifecycleDecision {
        LifecycleDecision::Exit {
            reason,
            watched,
            stalled,
        }
    }

    #[test]
    fn armed_tap_stall_exits_at_budget_unless_tap_stops() {
        let mut watchdog = LifecycleWatchdog::default();
        let stalled = observation(TapPhase::Armed, false, Duration::ZERO);
        assert_eq!(
            watched(&mut watchdog, Duration::from_nanos(1_499_999_999), stalled),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watched(&mut watchdog, TAP_SHUTDOWN_BUDGET, stalled),
            exit(
                LifecycleExitReason::TapThreadStalled,
                TAP_SHUTDOWN_BUDGET,
                TAP_SHUTDOWN_BUDGET
            )
        );

        let mut completed = LifecycleWatchdog::default();
        let _ = completed.evaluate(Duration::ZERO, stalled);
        assert_eq!(
            completed.evaluate(
                LIFECYCLE_POLL_INTERVAL,
                observation(TapPhase::TapStopped, false, Duration::ZERO)
            ),
            LifecycleDecision::Complete
        );
    }

    #[test]
    fn a_slow_capability_probe_is_not_a_wedged_tap_thread() {
        // #952: a between-slice `has_accessibility` probe (a WindowServer
        // round trip) that took ~1.6 s around a sleep transition was charged
        // against the 1.5 s stall budget and force-exited the agent. The
        // probe has its own budget.
        let mut watchdog = LifecycleWatchdog::default();
        let probing = observation(TapPhase::Probing, false, Duration::ZERO);
        assert_eq!(
            watched(&mut watchdog, TAP_SHUTDOWN_BUDGET, probing),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watched(&mut watchdog, Duration::from_millis(1_740), probing),
            LifecycleDecision::Continue
        );
        // The probe returning re-marks progress; the tap is healthy again.
        assert_eq!(
            watched(
                &mut watchdog,
                Duration::from_millis(1_800),
                observation(TapPhase::Armed, false, Duration::from_millis(1_750))
            ),
            LifecycleDecision::Continue
        );

        // A probe that never returns is still the freeze hazard.
        let mut wedged = LifecycleWatchdog::default();
        assert_eq!(
            watched(&mut wedged, TAP_PROBE_BUDGET, probing),
            exit(
                LifecycleExitReason::TapThreadStalled,
                TAP_PROBE_BUDGET,
                TAP_PROBE_BUDGET
            )
        );
    }

    #[test]
    fn a_stop_request_landing_on_a_probe_waits_for_the_probe_budget() {
        // Releasing the hook when the session or display gate closes lands the
        // stop exactly when the probes are slowest; the shorter stop budget
        // would kill the agent for the teardown it just asked for.
        let mut watchdog = LifecycleWatchdog::default();
        let probing = observation(TapPhase::Probing, true, Duration::ZERO);
        assert_eq!(
            watched(&mut watchdog, TAP_SHUTDOWN_BUDGET, probing),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watched(&mut watchdog, TAP_PROBE_BUDGET, probing),
            exit(
                LifecycleExitReason::StopTimedOut,
                TAP_PROBE_BUDGET,
                TAP_PROBE_BUDGET
            )
        );
    }

    #[test]
    fn a_probe_return_between_watchdog_polls_refreshes_the_stall() {
        let signals = WatchdogSignals {
            origin: Instant::now().checked_sub(Duration::from_secs(2)).unwrap(),
            ..WatchdogSignals::default()
        };
        let mut watchdog = LifecycleWatchdog::default();
        assert_eq!(
            watchdog.evaluate(
                Duration::ZERO,
                observation(TapPhase::Armed, false, signals.tap_progress_at())
            ),
            LifecycleDecision::Continue
        );
        signals.set_phase(TapPhase::Probing);
        let probe_returned = signals.now();
        signals.mark_armed();

        // No poll saw `Probing`, so the fresh mark must distinguish this
        // return from an armed tap that stalled for the whole interval.
        assert_eq!(
            watchdog.evaluate(
                probe_returned,
                observation(signals.phase(), false, signals.tap_progress_at())
            ),
            LifecycleDecision::Continue
        );
    }

    #[test]
    fn probe_teardown_keeps_the_short_budget_with_or_without_a_stop() {
        for (stop_requested, reason) in [
            (false, LifecycleExitReason::TapThreadStalled),
            (true, LifecycleExitReason::StopTimedOut),
        ] {
            let signals = WatchdogSignals {
                origin: Instant::now().checked_sub(Duration::from_secs(2)).unwrap(),
                ..WatchdogSignals::default()
            };
            signals.set_phase(TapPhase::Probing);
            if stop_requested {
                signals.request_stop();
            }
            let probing = observation(
                signals.phase(),
                signals.stop_requested(),
                signals.tap_progress_at(),
            );
            signals.mark_armed();
            let teardown = observation(
                signals.phase(),
                signals.stop_requested(),
                signals.tap_progress_at(),
            );
            let probe_returned = teardown.tap_progress_at;
            let probe_duration = Duration::from_millis(1_600);
            let mut watchdog = LifecycleWatchdog::default();
            assert_eq!(
                watchdog.evaluate(probe_returned.checked_sub(probe_duration).unwrap(), probing),
                LifecycleDecision::Continue
            );
            assert_eq!(
                watched(&mut watchdog, probe_returned, probing),
                LifecycleDecision::Continue
            );
            assert_eq!(
                watchdog.evaluate(probe_returned, teardown),
                LifecycleDecision::Continue,
                "a completed probe must not consume the teardown budget"
            );
            let deadline = probe_returned + TAP_SHUTDOWN_BUDGET;
            assert_eq!(
                watched(
                    &mut watchdog,
                    deadline.checked_sub(LIFECYCLE_POLL_INTERVAL).unwrap(),
                    teardown
                ),
                LifecycleDecision::Continue
            );
            let stalled = if stop_requested {
                probe_duration + TAP_SHUTDOWN_BUDGET
            } else {
                TAP_SHUTDOWN_BUDGET
            };
            assert_eq!(
                watched(&mut watchdog, deadline, teardown),
                exit(reason, TAP_SHUTDOWN_BUDGET, stalled),
                "teardown must not retain the probe budget"
            );
        }
    }

    #[test]
    fn a_teardown_after_a_slow_probe_gets_the_short_stop_budget_afresh() {
        // A stop that waited out a 1.6 s probe has already outlived the short
        // budget when the probe returns. The teardown that follows (a few
        // CoreGraphics calls, then the tap is destroyed) is what the short
        // budget is for, so it starts over there instead of exiting the agent
        // before the teardown can begin.
        let mut watchdog = LifecycleWatchdog::default();
        let probing = observation(TapPhase::Probing, true, Duration::ZERO);
        let probe_returned = Duration::from_millis(1_600);
        assert_eq!(
            watched(&mut watchdog, probe_returned, probing),
            LifecycleDecision::Continue
        );
        // The thread re-arms, notices the stop at the top of the loop, and
        // starts the synchronous teardown.
        let back_in_the_loop = probe_returned + LIFECYCLE_POLL_INTERVAL;
        assert_eq!(
            watchdog.evaluate(
                back_in_the_loop,
                observation(TapPhase::Armed, true, probe_returned)
            ),
            LifecycleDecision::Continue,
            "1.6 s under the probe budget does not exhaust the stop budget"
        );
        let tap_stopped = observation(TapPhase::TapStopped, true, probe_returned);
        // The short budget counts from the last tick before the probe was seen
        // to have returned: the tick that sees the return charges the whole gap
        // it straddles, so this is conservative by at most one poll interval.
        let teardown_deadline = probe_returned + TAP_SHUTDOWN_BUDGET;
        assert_eq!(
            watched(
                &mut watchdog,
                probe_returned + Duration::from_millis(1_400),
                tap_stopped
            ),
            LifecycleDecision::Continue
        );
        // The thread must still exit after the tap is destroyed, on the short
        // budget counted from leaving the probe.
        assert_eq!(
            watched(&mut watchdog, teardown_deadline, tap_stopped),
            exit(
                LifecycleExitReason::StopTimedOut,
                TAP_SHUTDOWN_BUDGET,
                teardown_deadline
            )
        );
    }

    #[test]
    fn a_frozen_process_is_not_a_wedged_tap_thread() {
        // #952, the other half: around a sleep transition the watchdog thread
        // itself has fired 1.0 s and 4.8 s past its budget, so it was not
        // running for at least that long — and neither was the tap thread it
        // judges. The freeze they shared is not charged to the tap thread.
        let mut watchdog = LifecycleWatchdog::default();
        let before_freeze = observation(TapPhase::Armed, false, Duration::ZERO);
        let _ = watched(&mut watchdog, Duration::from_millis(300), before_freeze);
        // The 11:54:19 exit on the reporting host: 2515 ms since the mark,
        // with the kernel's System Sleep and System Wake inside the gap.
        let thawed = Duration::from_millis(2_815);
        let after_thaw = after_sleep(before_freeze);
        assert_eq!(
            watchdog.evaluate(thawed, after_thaw),
            LifecycleDecision::Continue,
            "300 ms watched + one capped gap is under budget"
        );
        // The thawed tap thread marks progress on its next slice boundary.
        assert_eq!(
            watched(
                &mut watchdog,
                thawed + TAP_SHUTDOWN_BUDGET,
                after_sleep(observation(
                    TapPhase::Armed,
                    false,
                    thawed + Duration::from_millis(40)
                ))
            ),
            LifecycleDecision::Continue
        );

        // A tap thread that stays silent after the thaw is still wedged: the
        // budget runs out once the stall the watchdog saw reaches it.
        let mut wedged = LifecycleWatchdog::default();
        let _ = watched(&mut wedged, Duration::from_millis(300), before_freeze);
        let _ = wedged.evaluate(thawed, after_thaw);
        // 1.5 s budget, less the 300 ms watched and the 500 ms capped gap.
        let remaining = Duration::from_millis(700);
        assert_eq!(
            watched(&mut wedged, thawed + remaining, after_thaw),
            exit(
                LifecycleExitReason::TapThreadStalled,
                TAP_SHUTDOWN_BUDGET,
                thawed + remaining
            )
        );
    }

    #[test]
    fn a_late_poll_within_the_observation_gap_still_counts() {
        // Scheduling jitter up to the gap is ordinary watching: the stall it
        // spans is charged in full, so a wedged tap gains no time from it.
        let mut watchdog = LifecycleWatchdog::default();
        let stalled = observation(TapPhase::Armed, false, Duration::ZERO);
        let _ = watched(&mut watchdog, Duration::from_millis(1_000), stalled);
        let late = Duration::from_millis(1_000) + OBSERVATION_GAP;
        assert_eq!(
            watchdog.evaluate(late, stalled),
            exit(LifecycleExitReason::TapThreadStalled, late, late)
        );
    }

    #[test]
    fn a_gap_with_no_sleep_in_it_is_charged_in_full() {
        // A watchdog delayed by scheduling alone, with the tap thread wedged
        // the whole time, must not read the delay as a freeze: without a
        // kernel sleep or wake in the gap every millisecond counts, and the
        // exit lands on the first poll past the budget.
        let mut watchdog = LifecycleWatchdog::default();
        let stalled = observation(TapPhase::Armed, false, Duration::ZERO);
        assert_eq!(
            watchdog.evaluate(Duration::ZERO, stalled),
            LifecycleDecision::Continue
        );
        let delayed = Duration::from_secs(3);
        assert_eq!(
            watchdog.evaluate(delayed, stalled),
            exit(LifecycleExitReason::TapThreadStalled, delayed, delayed)
        );
    }

    #[test]
    fn repeated_sleep_cycles_still_accumulate_a_wedged_tap() {
        // The cap discounts each transition gap, never all of it: a tap thread
        // wedged across a DarkWake cycle is still caught after a few cycles.
        let mut watchdog = LifecycleWatchdog::default();
        let cycle = Duration::from_secs(3);
        let mut stalled = observation(TapPhase::Armed, false, Duration::ZERO);
        assert_eq!(
            watchdog.evaluate(Duration::ZERO, stalled),
            LifecycleDecision::Continue
        );
        for n in 1..3 {
            stalled = after_sleep(stalled);
            assert_eq!(
                watchdog.evaluate(cycle * n, stalled),
                LifecycleDecision::Continue,
                "cycle {n}: {n} capped gaps are under budget"
            );
        }
        stalled = after_sleep(stalled);
        assert_eq!(
            watchdog.evaluate(cycle * 3, stalled),
            exit(
                LifecycleExitReason::TapThreadStalled,
                OBSERVATION_GAP * 3,
                cycle * 3
            )
        );
    }

    #[test]
    fn a_stop_that_straddles_a_freeze_gets_a_watched_budget() {
        let mut watchdog = LifecycleWatchdog::default();
        let stopping = observation(TapPhase::Armed, true, Duration::ZERO);
        let _ = watched(&mut watchdog, Duration::from_millis(200), stopping);
        let thawed = Duration::from_secs(5);
        let after_thaw = after_sleep(stopping);
        assert_eq!(
            watchdog.evaluate(thawed, after_thaw),
            LifecycleDecision::Continue
        );
        // 1.5 s budget, less the 200 ms watched and the 500 ms capped gap.
        let remaining = Duration::from_millis(800);
        assert_eq!(
            watched(&mut watchdog, thawed + remaining, after_thaw),
            exit(
                LifecycleExitReason::StopTimedOut,
                TAP_SHUTDOWN_BUDGET,
                thawed + remaining
            )
        );
    }

    #[test]
    fn tap_creation_or_activation_stall_exits_at_budget() {
        let mut watchdog = LifecycleWatchdog::default();
        let arming = observation(TapPhase::Arming, false, Duration::ZERO);
        assert_eq!(
            watched(&mut watchdog, Duration::from_nanos(1_499_999_999), arming),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watched(&mut watchdog, TAP_SHUTDOWN_BUDGET, arming),
            exit(
                LifecycleExitReason::TapThreadStalled,
                TAP_SHUTDOWN_BUDGET,
                TAP_SHUTDOWN_BUDGET
            )
        );
    }

    #[test]
    fn stop_requires_thread_exit_even_after_tap_stops() {
        let mut watchdog = LifecycleWatchdog::default();
        let _ = watchdog.evaluate(
            Duration::ZERO,
            observation(TapPhase::Armed, true, Duration::ZERO),
        );
        let tap_stopped = observation(TapPhase::TapStopped, true, Duration::ZERO);
        assert_eq!(
            watched(&mut watchdog, Duration::from_millis(500), tap_stopped),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watched(&mut watchdog, TAP_SHUTDOWN_BUDGET, tap_stopped),
            exit(
                LifecycleExitReason::StopTimedOut,
                TAP_SHUTDOWN_BUDGET,
                TAP_SHUTDOWN_BUDGET
            )
        );

        let mut completed = LifecycleWatchdog::default();
        let _ = completed.evaluate(
            Duration::ZERO,
            observation(TapPhase::Armed, true, Duration::ZERO),
        );
        assert_eq!(
            completed.evaluate(
                LIFECYCLE_POLL_INTERVAL,
                observation(TapPhase::ThreadExited, true, Duration::ZERO)
            ),
            LifecycleDecision::Complete
        );
    }

    #[test]
    fn starting_and_healthy_states_never_time_out() {
        let mut watchdog = LifecycleWatchdog::default();
        let starting = observation(TapPhase::Starting, false, Duration::ZERO);
        assert_eq!(
            watched(&mut watchdog, TAP_SHUTDOWN_BUDGET * 2, starting),
            LifecycleDecision::Continue
        );
        // Progress marked every slice keeps an armed tap healthy indefinitely.
        let mut progress = TAP_SHUTDOWN_BUDGET * 2;
        while progress < TAP_SHUTDOWN_BUDGET * 4 {
            progress += Duration::from_millis(500);
            assert_eq!(
                watched(
                    &mut watchdog,
                    progress,
                    observation(TapPhase::Armed, false, progress)
                ),
                LifecycleDecision::Continue
            );
        }
        assert_eq!(
            watchdog.evaluate(
                progress + LIFECYCLE_POLL_INTERVAL,
                observation(TapPhase::ThreadExited, false, Duration::ZERO)
            ),
            LifecycleDecision::Complete
        );
    }

    #[test]
    fn a_burst_of_re_arms_is_allowed_and_then_bounded() {
        let mut budget = RearmBudget::default();
        for i in 0..REARM_LIMIT {
            assert!(
                budget.allow(Duration::from_millis(u64::from(i) * 500)),
                "re-arm {i} is within the budget"
            );
        }
        assert!(!budget.allow(Duration::from_millis(u64::from(REARM_LIMIT) * 500)));
    }

    #[test]
    fn a_new_window_restores_the_budget() {
        let mut budget = RearmBudget::default();
        for _ in 0..=REARM_LIMIT {
            let _ = budget.allow(Duration::ZERO);
        }
        assert!(!budget.allow(Duration::ZERO));
        // A re-arm past the window opens a fresh one, anchored at that re-arm
        // rather than at the first one ever charged.
        for _ in 0..REARM_LIMIT {
            assert!(budget.allow(REARM_WINDOW));
        }
        let just_inside = REARM_WINDOW + REARM_WINDOW.saturating_sub(Duration::from_millis(1));
        assert!(!budget.allow(just_inside));
    }

    fn epoch(n: i64) -> PowerEpoch {
        PowerEpoch {
            slept_us: n,
            woke_us: n,
        }
    }

    #[test]
    fn the_first_poll_is_charged_from_spawn() {
        // The thread sleeps before its first poll. A callback that wedged in
        // that window, with the poll itself delayed past the budget, exits on
        // that poll rather than being granted a second budget.
        let mut watchdog = CallbackWatchdog::watching_since(0, epoch(0));
        assert_eq!(
            watchdog.evaluate(300, Some(5), epoch(0)),
            Some(StuckCallback {
                watched: Duration::from_millis(295),
                stalled: Duration::from_millis(295),
            })
        );
        // Unless the kernel slept and woke inside that first gap.
        let mut slept = CallbackWatchdog::watching_since(0, epoch(0));
        assert_eq!(slept.evaluate(300, Some(5), epoch(1)), None);
    }

    #[test]
    fn callback_timeout_keeps_the_200ms_boundary() {
        // Warm polling, then a callback entered 5 ms before a poll and never
        // left: the exit lands on the first poll at or past the budget.
        let mut watchdog = CallbackWatchdog::default();
        let poll = u64::try_from(CALLBACK_POLL_INTERVAL.as_millis()).unwrap();
        for tick in 0..=50 {
            assert_eq!(watchdog.evaluate(tick * poll, None, epoch(0)), None);
        }
        let entered = 50 * poll + 15;
        let mut now = 51 * poll;
        while now.saturating_sub(entered) < 200 {
            assert_eq!(
                watchdog.evaluate(now, Some(entered), epoch(0)),
                None,
                "{} ms inside the callback is within budget",
                now - entered
            );
            now += poll;
        }
        assert_eq!(
            watchdog.evaluate(now, Some(entered), epoch(0)),
            Some(StuckCallback {
                watched: Duration::from_millis(now - entered),
                stalled: Duration::from_millis(now - entered),
            })
        );
        assert_eq!(now - entered, 205, "budget + the poll that sees it");

        // A fresh high-frequency event must not inherit an older entry time.
        let mut fresh = CallbackWatchdog::default();
        assert_eq!(fresh.evaluate(9_800, Some(9_600), epoch(0)), None);
        assert_eq!(fresh.evaluate(9_820, Some(9_801), epoch(0)), None);
    }

    #[test]
    fn a_frozen_process_is_not_a_stuck_callback() {
        // The same freeze that leaves the lifecycle watchdog unscheduled leaves
        // this thread and the tap thread unscheduled: an entry from before the
        // freeze is charged at most one capped gap for a gap the kernel slept
        // and woke in.
        let mut watchdog = CallbackWatchdog::default();
        let poll = u64::try_from(CALLBACK_POLL_INTERVAL.as_millis()).unwrap();
        for tick in 0..=50 {
            assert_eq!(watchdog.evaluate(tick * poll, None, epoch(0)), None);
        }
        let entered = 50 * poll + 15;
        assert_eq!(watchdog.evaluate(51 * poll, Some(entered), epoch(0)), None);
        let thawed = 51 * poll + 2_500;
        assert_eq!(
            watchdog.evaluate(thawed, Some(entered), epoch(1)),
            None,
            "5 ms watched + one capped gap is under budget"
        );
        // The thawed callback returns; the next entry starts over.
        assert_eq!(watchdog.evaluate(thawed + poll, None, epoch(1)), None);
        assert_eq!(
            watchdog.evaluate(thawed + 2 * poll, Some(thawed + poll + 3), epoch(1)),
            None
        );

        // A callback that stays entered after the thaw is still stuck.
        let mut wedged = CallbackWatchdog::default();
        for tick in 0..=50 {
            assert_eq!(wedged.evaluate(tick * poll, None, epoch(0)), None);
        }
        assert_eq!(wedged.evaluate(51 * poll, Some(entered), epoch(0)), None);
        assert_eq!(wedged.evaluate(thawed, Some(entered), epoch(1)), None);
        // 200 ms budget, less the 5 ms watched and the 100 ms capped gap.
        let mut now = thawed;
        for _ in 0..4 {
            now += poll;
            assert_eq!(wedged.evaluate(now, Some(entered), epoch(1)), None);
        }
        now += poll;
        assert_eq!(
            wedged.evaluate(now, Some(entered), epoch(1)),
            Some(StuckCallback {
                watched: Duration::from_millis(205),
                stalled: Duration::from_millis(now - entered),
            })
        );
    }

    #[test]
    fn a_callback_gap_with_no_sleep_in_it_is_charged_in_full() {
        // A poll delayed by scheduling alone, with the callback stuck the whole
        // time, is not a freeze: the exit lands on that poll.
        let mut watchdog = CallbackWatchdog::default();
        assert_eq!(watchdog.evaluate(0, Some(0), epoch(0)), None);
        assert_eq!(
            watchdog.evaluate(3_000, Some(0), epoch(0)),
            Some(StuckCallback {
                watched: Duration::from_millis(3_000),
                stalled: Duration::from_millis(3_000),
            })
        );
    }

    #[test]
    fn repeated_sleep_cycles_still_accumulate_a_stuck_callback() {
        let mut watchdog = CallbackWatchdog::default();
        assert_eq!(watchdog.evaluate(0, Some(0), epoch(0)), None);
        assert_eq!(watchdog.evaluate(3_000, Some(0), epoch(1)), None);
        assert_eq!(
            watchdog.evaluate(6_000, Some(0), epoch(2)),
            Some(StuckCallback {
                watched: CALLBACK_OBSERVATION_GAP * 2,
                stalled: Duration::from_millis(6_000),
            })
        );
    }
}
