//! When a module that stopped starts again: the sans-I/O restart policy.

use std::time::Duration;

use backon::{BackoffBuilder as _, ExponentialBackoff, ExponentialBuilder};

use super::ModuleTimings;

/// How many runs in a row may end without `ready` before the module is
/// marked failed.
pub(super) const MAX_FAILED_READIES: u32 = 5;

/// What follows a run's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AfterExit {
    /// Start the module again after this delay.
    RestartIn(Duration),
    /// Leave it stopped until the user turns it off and on or its folder
    /// changes.
    Failed,
}

/// One module's restart history: the doubling delay between restarts, and
/// how many runs in a row ended before `ready`.
#[derive(Debug)]
pub(super) struct Restarts {
    timings: ModuleTimings,
    backoff: ExponentialBackoff,
    failed_readies: u32,
}

impl Restarts {
    pub(super) fn new(timings: ModuleTimings) -> Self {
        Self {
            timings,
            backoff: backoff(&timings),
            failed_readies: 0,
        }
    }

    /// Note that a run ended, having been ready for `ready_for` (`None`
    /// when it never sent `ready`), and say what follows.
    ///
    /// A run that never got ready counts towards [`MAX_FAILED_READIES`]; a
    /// ready one clears the count. A run that stayed ready for
    /// [`ModuleTimings::healthy_after`] starts the delay over from
    /// [`ModuleTimings::first_restart`].
    pub(super) fn ended(&mut self, ready_for: Option<Duration>) -> AfterExit {
        match ready_for {
            None => {
                self.failed_readies += 1;
                if self.failed_readies >= MAX_FAILED_READIES {
                    return AfterExit::Failed;
                }
            }
            Some(ran) => {
                self.failed_readies = 0;
                if ran >= self.timings.healthy_after {
                    self.backoff = backoff(&self.timings);
                }
            }
        }
        AfterExit::RestartIn(self.backoff.next().unwrap_or(self.timings.max_restart))
    }
}

/// A fresh doubling delay: [`ModuleTimings::first_restart`], twice that,
/// and so on up to [`ModuleTimings::max_restart`], without end.
fn backoff(timings: &ModuleTimings) -> ExponentialBackoff {
    ExponentialBuilder::new()
        .with_min_delay(timings.first_restart)
        .with_max_delay(timings.max_restart)
        .without_max_times()
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn delays_double_from_one_second_to_a_minute() {
        let mut restarts = Restarts::new(ModuleTimings::default());
        let delays: Vec<_> = (0..9).map(|_| restarts.ended(Some(secs(5)))).collect();
        assert_eq!(
            delays,
            [1, 2, 4, 8, 16, 32, 60, 60, 60].map(|n| AfterExit::RestartIn(secs(n)))
        );
    }

    #[test]
    fn a_minute_of_healthy_running_starts_the_delay_over() {
        let mut restarts = Restarts::new(ModuleTimings::default());
        for _ in 0..4 {
            restarts.ended(Some(secs(1)));
        }
        assert_eq!(
            restarts.ended(Some(secs(59))),
            AfterExit::RestartIn(secs(16))
        );
        assert_eq!(
            restarts.ended(Some(secs(60))),
            AfterExit::RestartIn(secs(1))
        );
        assert_eq!(restarts.ended(Some(secs(1))), AfterExit::RestartIn(secs(2)));
    }

    #[test]
    fn five_runs_in_a_row_without_ready_fail_the_module() {
        let mut restarts = Restarts::new(ModuleTimings::default());
        for n in [1, 2, 4, 8] {
            assert_eq!(restarts.ended(None), AfterExit::RestartIn(secs(n)));
        }
        assert_eq!(restarts.ended(None), AfterExit::Failed);
    }

    #[test]
    fn a_ready_run_clears_the_count_of_failed_starts() {
        let mut restarts = Restarts::new(ModuleTimings::default());
        for _ in 0..4 {
            restarts.ended(None);
        }
        assert!(matches!(
            restarts.ended(Some(secs(1))),
            AfterExit::RestartIn(_)
        ));
        for _ in 0..4 {
            assert!(matches!(restarts.ended(None), AfterExit::RestartIn(_)));
        }
        assert_eq!(restarts.ended(None), AfterExit::Failed);
    }
}
