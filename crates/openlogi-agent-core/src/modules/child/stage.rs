//! Where one run stands, as one [`RunStage`], and [`RunStage::step`], the
//! only writer of it.

use std::time::Duration;

use tokio::time::Instant;

/// Where a run stands. Each stage holds only what is true in it: a ready
/// deadline only while the module has not answered, and the exit only once
/// it has exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RunStage {
    /// The module has not answered `hello`; past `deadline` it is killed.
    Starting { deadline: Instant },
    /// The module answered `hello` at `since`.
    Ready { since: Instant },
    /// The module exited as `status` says. What it already wrote is read
    /// until `until`; `since` is when it answered `hello`, if it did.
    Draining {
        status: String,
        until: Instant,
        since: Option<Instant>,
    },
}

/// What happens to a run, for [`RunStage::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StageEvent {
    /// The module answered `hello` with this host's protocol.
    Ready,
    /// The module exited as `status` says; its output is read for `grace`
    /// more at most.
    Exited { status: String, grace: Duration },
}

/// An event the run's stage has no edge for. The stage is left as it was.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{event} refused while {stage}")]
pub(super) struct Refused {
    pub(super) stage: &'static str,
    pub(super) event: &'static str,
}

impl RunStage {
    pub(super) fn starting(deadline: Instant) -> Self {
        Self::Starting { deadline }
    }

    /// Apply `event` at `now`: the one place a run's stage changes. A
    /// `ready` from a module that already exited, or a second one, is
    /// refused.
    pub(super) fn step(&mut self, event: StageEvent, now: Instant) -> Result<(), Refused> {
        let next = match (&*self, event) {
            (Self::Starting { .. }, StageEvent::Ready) => Self::Ready { since: now },
            (Self::Starting { .. }, StageEvent::Exited { status, grace }) => Self::Draining {
                status,
                until: now + grace,
                since: None,
            },
            (Self::Ready { since }, StageEvent::Exited { status, grace }) => Self::Draining {
                status,
                until: now + grace,
                since: Some(*since),
            },
            (stage, event) => {
                return Err(Refused {
                    stage: stage.name(),
                    event: match event {
                        StageEvent::Ready => "ready",
                        StageEvent::Exited { .. } => "exit",
                    },
                });
            }
        };
        *self = next;
        Ok(())
    }

    /// When the run ends unless something ends it first: the ready deadline
    /// while starting, the end of the drain once the module exited.
    pub(super) fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Starting { deadline } => Some(*deadline),
            Self::Ready { .. } => None,
            Self::Draining { until, .. } => Some(*until),
        }
    }

    /// Whether the module is to hear its keys and presses: it answered and
    /// is still running.
    pub(super) fn takes_input(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// Whether the module answered `hello`, and so may change faces, even
    /// if it has exited since.
    pub(super) fn answered(&self) -> bool {
        matches!(
            self,
            Self::Ready { .. } | Self::Draining { since: Some(_), .. }
        )
    }

    /// How the module exited, once it has.
    pub(super) fn exit(&self) -> Option<&str> {
        match self {
            Self::Draining { status, .. } => Some(status),
            Self::Starting { .. } | Self::Ready { .. } => None,
        }
    }

    /// How long the module has been ready at `now`; `None` when it never
    /// answered.
    pub(super) fn ready_for(&self, now: Instant) -> Option<Duration> {
        match self {
            Self::Ready { since }
            | Self::Draining {
                since: Some(since), ..
            } => Some(now.duration_since(*since)),
            Self::Starting { .. } | Self::Draining { since: None, .. } => None,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Starting { .. } => "starting",
            Self::Ready { .. } => "ready",
            Self::Draining { .. } => "draining",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(2);

    fn exited() -> StageEvent {
        StageEvent::Exited {
            status: "exited with status 3".into(),
            grace: GRACE,
        }
    }

    /// Every edge a run's stage has, and the ones it refuses: a second
    /// ready, a ready from a module that already exited, and a second
    /// exit, each leaving the stage as it was.
    #[test]
    fn a_run_moves_from_starting_to_ready_to_draining_and_refuses_the_rest() {
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        let mut stage = RunStage::starting(start + Duration::from_secs(10));
        assert_eq!(stage.deadline(), Some(start + Duration::from_secs(10)));
        assert!(!stage.takes_input() && !stage.answered());
        assert_eq!(stage.ready_for(later), None);

        stage.step(StageEvent::Ready, start).unwrap();
        assert_eq!(stage, RunStage::Ready { since: start });
        assert_eq!(stage.deadline(), None, "a ready run has no deadline");
        assert!(stage.takes_input() && stage.answered());
        assert_eq!(stage.ready_for(later), Some(Duration::from_secs(1)));
        assert_eq!(
            stage.step(StageEvent::Ready, later),
            Err(Refused {
                stage: "ready",
                event: "ready"
            })
        );
        assert_eq!(stage, RunStage::Ready { since: start });

        stage.step(exited(), later).unwrap();
        assert_eq!(
            stage,
            RunStage::Draining {
                status: "exited with status 3".into(),
                until: later + GRACE,
                since: Some(start),
            }
        );
        assert_eq!(stage.deadline(), Some(later + GRACE));
        assert!(!stage.takes_input(), "an exited module hears nothing");
        assert!(
            stage.answered(),
            "what it drew before it exited still counts"
        );
        assert_eq!(stage.exit(), Some("exited with status 3"));
        for (event, name) in [(StageEvent::Ready, "ready"), (exited(), "exit")] {
            let before = stage.clone();
            assert_eq!(
                stage.step(event, later),
                Err(Refused {
                    stage: "draining",
                    event: name
                }),
                "{name} while draining"
            );
            assert_eq!(stage, before);
        }
    }

    /// A module that exits before it answers drains without having been
    /// ready, and its late `ready` is refused: the run never reports it.
    #[test]
    fn a_ready_read_after_the_module_exited_is_refused() {
        let start = Instant::now();
        let mut stage = RunStage::starting(start + Duration::from_secs(10));
        stage.step(exited(), start).unwrap();
        assert!(!stage.answered());
        assert_eq!(stage.ready_for(start + GRACE), None);
        assert_eq!(
            stage.step(StageEvent::Ready, start),
            Err(Refused {
                stage: "draining",
                event: "ready"
            })
        );
        assert!(!stage.answered(), "still never ready");
    }
}
