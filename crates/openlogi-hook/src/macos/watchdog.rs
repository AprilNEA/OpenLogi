//! Testable state and timing for the macOS HID tap safety watchdogs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(super) const CALLBACK_STUCK_BUDGET: Duration = Duration::from_millis(200);
pub(super) const TAP_SHUTDOWN_BUDGET: Duration = Duration::from_millis(1_500);
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
            3 => Self::TapStopped,
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
    // the system sleeps so resume cannot consume either watchdog budget.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LifecycleObservation {
    pub phase: TapPhase,
    pub stop_requested: bool,
    pub tap_progress_at: Duration,
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
        elapsed: Duration,
    },
}

#[derive(Debug, Default)]
pub(super) struct LifecycleWatchdog {
    stop_at: Option<Duration>,
}

impl LifecycleWatchdog {
    pub fn evaluate(
        &mut self,
        now: Duration,
        observation: LifecycleObservation,
    ) -> LifecycleDecision {
        match observation.phase {
            TapPhase::Starting => return LifecycleDecision::Continue,
            TapPhase::ThreadExited => return LifecycleDecision::Complete,
            TapPhase::Arming | TapPhase::Armed | TapPhase::TapStopped => {}
        }

        if observation.stop_requested {
            self.stop_at.get_or_insert(now);
        }
        if observation.phase == TapPhase::TapStopped && !observation.stop_requested {
            return LifecycleDecision::Complete;
        }

        let timeout = if let Some(stopped) = self.stop_at {
            Some((LifecycleExitReason::StopTimedOut, stopped))
        } else if matches!(observation.phase, TapPhase::Arming | TapPhase::Armed) {
            Some((
                LifecycleExitReason::TapThreadStalled,
                observation.tap_progress_at,
            ))
        } else {
            None
        };
        let Some((reason, started)) = timeout else {
            return LifecycleDecision::Continue;
        };
        let elapsed = now.saturating_sub(started);
        if elapsed >= TAP_SHUTDOWN_BUDGET {
            LifecycleDecision::Exit { reason, elapsed }
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

pub(super) fn stuck_callback(now_ms: u64, entered_at_ms: u64) -> Option<Duration> {
    let elapsed = Duration::from_millis(now_ms.saturating_sub(entered_at_ms));
    (elapsed >= CALLBACK_STUCK_BUDGET).then_some(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_phase_decoder_is_total_and_fails_safe() {
        assert_eq!(TapPhase::decode(0), TapPhase::Starting);
        assert_eq!(TapPhase::decode(1), TapPhase::Arming);
        assert_eq!(TapPhase::decode(2), TapPhase::Armed);
        assert_eq!(TapPhase::decode(3), TapPhase::TapStopped);
        assert_eq!(TapPhase::decode(4), TapPhase::ThreadExited);
        assert_eq!(TapPhase::decode(5), TapPhase::Armed);
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
        }
    }

    #[test]
    fn armed_tap_stall_exits_at_budget_unless_tap_stops() {
        let mut watchdog = LifecycleWatchdog::default();
        assert_eq!(
            watchdog.evaluate(
                Duration::ZERO,
                observation(TapPhase::Armed, false, Duration::ZERO)
            ),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(
                Duration::from_nanos(1_499_999_999),
                observation(TapPhase::Armed, false, Duration::ZERO)
            ),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(
                TAP_SHUTDOWN_BUDGET,
                observation(TapPhase::Armed, false, Duration::ZERO)
            ),
            LifecycleDecision::Exit {
                reason: LifecycleExitReason::TapThreadStalled,
                elapsed: TAP_SHUTDOWN_BUDGET,
            }
        );

        let mut completed = LifecycleWatchdog::default();
        let _ = completed.evaluate(
            Duration::ZERO,
            observation(TapPhase::Armed, false, Duration::ZERO),
        );
        assert_eq!(
            completed.evaluate(
                Duration::from_millis(500),
                observation(TapPhase::TapStopped, false, Duration::ZERO)
            ),
            LifecycleDecision::Complete
        );
    }

    #[test]
    fn tap_creation_or_activation_stall_exits_at_budget() {
        let mut watchdog = LifecycleWatchdog::default();
        assert_eq!(
            watchdog.evaluate(
                Duration::from_nanos(1_499_999_999),
                observation(TapPhase::Arming, false, Duration::ZERO)
            ),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(
                TAP_SHUTDOWN_BUDGET,
                observation(TapPhase::Arming, false, Duration::ZERO)
            ),
            LifecycleDecision::Exit {
                reason: LifecycleExitReason::TapThreadStalled,
                elapsed: TAP_SHUTDOWN_BUDGET,
            }
        );
    }

    #[test]
    fn stop_requires_thread_exit_even_after_tap_stops() {
        let mut watchdog = LifecycleWatchdog::default();
        let _ = watchdog.evaluate(
            Duration::ZERO,
            observation(TapPhase::Armed, true, Duration::ZERO),
        );
        assert_eq!(
            watchdog.evaluate(
                Duration::from_millis(500),
                observation(TapPhase::TapStopped, true, Duration::ZERO)
            ),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(
                TAP_SHUTDOWN_BUDGET,
                observation(TapPhase::TapStopped, true, Duration::ZERO)
            ),
            LifecycleDecision::Exit {
                reason: LifecycleExitReason::StopTimedOut,
                elapsed: TAP_SHUTDOWN_BUDGET,
            }
        );

        let mut completed = LifecycleWatchdog::default();
        let _ = completed.evaluate(
            Duration::ZERO,
            observation(TapPhase::Armed, true, Duration::ZERO),
        );
        assert_eq!(
            completed.evaluate(
                Duration::from_millis(500),
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
            watchdog.evaluate(Duration::ZERO, starting),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(TAP_SHUTDOWN_BUDGET * 2, starting),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(
                TAP_SHUTDOWN_BUDGET * 3,
                observation(TapPhase::Armed, false, Duration::from_secs(4))
            ),
            LifecycleDecision::Continue
        );
        assert_eq!(
            watchdog.evaluate(
                TAP_SHUTDOWN_BUDGET * 4,
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

    #[test]
    fn callback_timeout_keeps_the_200ms_boundary() {
        assert_eq!(stuck_callback(200, 1), None);
        assert_eq!(stuck_callback(201, 1), Some(CALLBACK_STUCK_BUDGET));
        // A fresh high-frequency event must not inherit an older entry time.
        assert_eq!(stuck_callback(10_000, 9_801), None);
    }
}
