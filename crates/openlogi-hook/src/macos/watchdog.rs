//! Testable state and timing for the macOS HID tap safety watchdogs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(super) const CALLBACK_STUCK_BUDGET: Duration = Duration::from_millis(200);
pub(super) const TAP_SHUTDOWN_BUDGET: Duration = Duration::from_millis(1_500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(super) enum TapPhase {
    Starting,
    Armed,
    TapStopped,
    ThreadExited,
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
        match self.phase.load(Ordering::Acquire) {
            0 => TapPhase::Starting,
            1 => TapPhase::Armed,
            2 => TapPhase::TapStopped,
            3 => TapPhase::ThreadExited,
            _ => unreachable!("invalid tap lifecycle phase"),
        }
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
            TapPhase::Armed | TapPhase::TapStopped => {}
        }

        if observation.stop_requested {
            self.stop_at.get_or_insert(now);
        }
        if observation.phase == TapPhase::TapStopped && !observation.stop_requested {
            return LifecycleDecision::Complete;
        }

        let timeout = if let Some(stopped) = self.stop_at {
            Some((LifecycleExitReason::StopTimedOut, stopped))
        } else if observation.phase == TapPhase::Armed {
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

pub(super) fn stuck_callback(now_ms: u64, entered_at_ms: u64) -> Option<Duration> {
    let elapsed = Duration::from_millis(now_ms.saturating_sub(entered_at_ms));
    (elapsed >= CALLBACK_STUCK_BUDGET).then_some(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn callback_timeout_keeps_the_200ms_boundary() {
        assert_eq!(stuck_callback(200, 1), None);
        assert_eq!(stuck_callback(201, 1), Some(CALLBACK_STUCK_BUDGET));
        // A fresh high-frequency event must not inherit an older entry time.
        assert_eq!(stuck_callback(10_000, 9_801), None);
    }
}
