//! One module's life across its runs: where it stands, as one
//! [`ModuleLife`], and [`Module::step`], the only writer of it.

use openlogi_core::module::ModuleId;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::modules::ModuleTimings;
use crate::modules::child::{RunEnd, RunHandle};
use crate::modules::restart::{AfterExit, Restarts};

/// A run the host started and has not seen end.
pub(super) struct LiveRun {
    pub(super) handle: RunHandle,
    pub(super) generation: u64,
    /// The folder fingerprint and settings it started with: a change to
    /// either restarts it.
    pub(super) fingerprint: u64,
    pub(super) settings: toml::Table,
}

/// Where one module stands. Each stage holds only what is true in it: a
/// live run only while one runs, a restart time only while one waits, and
/// no error once a run got ready.
pub(super) enum ModuleLife {
    /// No run: start one as soon as the module is wanted. `error` is how
    /// the last run the host did not end ended, until a run gets ready.
    Idle { error: Option<String> },
    /// A run started and has not answered `hello` yet.
    Starting { run: LiveRun, error: Option<String> },
    /// The run answered `hello`.
    Running { run: LiveRun },
    /// The host told the run to leave and waits for it to end.
    Stopping { run: LiveRun, error: Option<String> },
    /// The last run ended on its own; the next starts at `at`, if the
    /// module is still wanted.
    Waiting { at: Instant, error: String },
    /// Too many runs in a row ended before `ready`: no run until the module
    /// is turned off and on or its folder fingerprint changes.
    Failed { fingerprint: u64, error: String },
}

/// What happens to a module, for [`Module::step`].
pub(super) enum LifeEvent {
    /// The host started `run`.
    Started(LiveRun),
    /// Run `generation` answered `hello`.
    Ready { generation: u64 },
    /// The host tells the live run to leave.
    Stop,
    /// Run `generation` ended, as `end` says.
    Ended { generation: u64, end: RunEnd },
    /// The user turned the module off: a failure or a pending restart is
    /// forgotten.
    TurnedOff,
    /// The module's folder has `fingerprint` now: a module that failed from
    /// another folder may start again.
    Folder { fingerprint: u64 },
}

/// An event the module's stage has no edge for. The module is left as it
/// was; a refused [`LifeEvent::Started`] drops its run, which shuts it down.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{event} refused while {stage}")]
pub(super) struct Refused {
    pub(super) stage: &'static str,
    pub(super) event: &'static str,
}

/// One module the host knows: its life, its restart history, and the
/// plan's count of turn-offs it last saw.
pub(super) struct Module {
    life: ModuleLife,
    restarts: Restarts,
    turned_off: u64,
    timings: ModuleTimings,
}

impl Module {
    pub(super) fn new(timings: ModuleTimings, turned_off: u64) -> Self {
        Self {
            life: ModuleLife::Idle { error: None },
            restarts: Restarts::new(timings),
            turned_off,
            timings,
        }
    }

    pub(super) fn life(&self) -> &ModuleLife {
        &self.life
    }

    /// The live run, in any stage that has one.
    pub(super) fn run(&self) -> Option<&LiveRun> {
        match &self.life {
            ModuleLife::Starting { run, .. }
            | ModuleLife::Running { run }
            | ModuleLife::Stopping { run, .. } => Some(run),
            ModuleLife::Idle { .. } | ModuleLife::Waiting { .. } | ModuleLife::Failed { .. } => {
                None
            }
        }
    }

    /// The run a press may go to: one that answered `hello` and was not
    /// told to leave.
    pub(super) fn running(&self) -> Option<&LiveRun> {
        match &self.life {
            ModuleLife::Running { run } => Some(run),
            _ => None,
        }
    }

    pub(super) fn stopping(&self) -> bool {
        matches!(self.life, ModuleLife::Stopping { .. })
    }

    /// Whether run `generation` may still change faces: it is live and was
    /// not told to leave.
    pub(super) fn takes_faces(&self, generation: u64) -> bool {
        match &self.life {
            ModuleLife::Starting { run, .. } | ModuleLife::Running { run } => {
                run.generation == generation
            }
            _ => false,
        }
    }

    /// Whether a run may start at `now`.
    pub(super) fn due(&self, now: Instant) -> bool {
        match self.life {
            ModuleLife::Idle { .. } => true,
            ModuleLife::Waiting { at, .. } => at <= now,
            _ => false,
        }
    }

    /// When the module waits to start again.
    pub(super) fn restart_at(&self) -> Option<Instant> {
        match self.life {
            ModuleLife::Waiting { at, .. } => Some(at),
            _ => None,
        }
    }

    /// How the last run the host did not end ended, until a run gets ready.
    pub(super) fn error(&self) -> Option<&str> {
        match &self.life {
            ModuleLife::Idle { error }
            | ModuleLife::Starting { error, .. }
            | ModuleLife::Stopping { error, .. } => error.as_deref(),
            ModuleLife::Waiting { error, .. } | ModuleLife::Failed { error, .. } => Some(error),
            ModuleLife::Running { .. } => None,
        }
    }

    /// Note the plan's count of turn-offs for this module, and say whether
    /// it moved since the last plan: the toggle that clears a failure, even
    /// when this plan already turns the module on again.
    pub(super) fn saw_turned_off(&mut self, count: u64) -> bool {
        std::mem::replace(&mut self.turned_off, count) != count
    }

    /// Apply `event` at `now`: the one place a module's stage changes.
    pub(super) fn step(
        &mut self,
        id: &ModuleId,
        event: LifeEvent,
        now: Instant,
    ) -> Result<(), Refused> {
        let stage = self.life.name();
        let refused = |event: &'static str| Refused { stage, event };
        let life = std::mem::replace(&mut self.life, ModuleLife::Idle { error: None });
        let (next, verdict) = match (life, event) {
            (ModuleLife::Idle { error }, LifeEvent::Started(run)) => {
                (ModuleLife::Starting { run, error }, Ok(()))
            }
            (ModuleLife::Waiting { at, error }, LifeEvent::Started(run)) if at <= now => (
                ModuleLife::Starting {
                    run,
                    error: Some(error),
                },
                Ok(()),
            ),
            (life, LifeEvent::Started(_)) => (life, Err(refused("start"))),

            (ModuleLife::Starting { run, .. }, LifeEvent::Ready { generation })
                if run.generation == generation =>
            {
                info!(module = %id, "module ready");
                (ModuleLife::Running { run }, Ok(()))
            }
            (ModuleLife::Stopping { run, .. }, LifeEvent::Ready { generation })
                if run.generation == generation =>
            {
                (ModuleLife::Stopping { run, error: None }, Ok(()))
            }
            (life, LifeEvent::Ready { .. }) => (life, Err(refused("ready"))),

            (ModuleLife::Starting { mut run, error }, LifeEvent::Stop) => {
                run.handle.stop();
                (ModuleLife::Stopping { run, error }, Ok(()))
            }
            (ModuleLife::Running { mut run }, LifeEvent::Stop) => {
                run.handle.stop();
                (ModuleLife::Stopping { run, error: None }, Ok(()))
            }
            (life @ ModuleLife::Stopping { .. }, LifeEvent::Stop) => (life, Ok(())),
            (life, LifeEvent::Stop) => (life, Err(refused("stop"))),

            (ModuleLife::Stopping { run, error }, LifeEvent::Ended { generation, .. })
                if run.generation == generation =>
            {
                info!(module = %id, "module stopped");
                (ModuleLife::Idle { error }, Ok(()))
            }
            (
                ModuleLife::Starting { run, .. } | ModuleLife::Running { run },
                LifeEvent::Ended { generation, end },
            ) if run.generation == generation => (self.after_exit(id, &end, &run, now), Ok(())),
            (life, LifeEvent::Ended { .. }) => (life, Err(refused("end"))),

            (ModuleLife::Waiting { .. } | ModuleLife::Failed { .. }, LifeEvent::TurnedOff) => {
                self.restarts = Restarts::new(self.timings);
                (ModuleLife::Idle { error: None }, Ok(()))
            }

            (
                ModuleLife::Failed { fingerprint, error },
                LifeEvent::Folder {
                    fingerprint: now_has,
                },
            ) if fingerprint != now_has => {
                info!(module = %id, "module folder changed: trying the failed module again");
                self.restarts = Restarts::new(self.timings);
                (ModuleLife::Idle { error: Some(error) }, Ok(()))
            }
            (life, LifeEvent::TurnedOff | LifeEvent::Folder { .. }) => (life, Ok(())),
        };
        self.life = next;
        verdict
    }

    /// The stage that follows a live run of `id` ending on its own at
    /// `now`, as `end` says.
    fn after_exit(
        &mut self,
        id: &ModuleId,
        end: &RunEnd,
        run: &LiveRun,
        now: Instant,
    ) -> ModuleLife {
        let error = end.error.clone().unwrap_or_else(|| "stopped".into());
        match self.restarts.ended(end.ready_for) {
            AfterExit::RestartIn(delay) => {
                warn!(module = %id, reason = %error, delay_ms = delay.as_millis(), "module stopped: restarting");
                ModuleLife::Waiting {
                    at: now + delay,
                    error,
                }
            }
            AfterExit::Failed => {
                warn!(module = %id, reason = %error, "module failed to start five times in a row: leaving it stopped until it is turned off and on or its folder changes");
                ModuleLife::Failed {
                    fingerprint: run.fingerprint,
                    error,
                }
            }
        }
    }
}

impl ModuleLife {
    fn name(&self) -> &'static str {
        match self {
            Self::Idle { .. } => "idle",
            Self::Starting { .. } => "starting",
            Self::Running { .. } => "running",
            Self::Stopping { .. } => "stopping",
            Self::Waiting { .. } => "waiting",
            Self::Failed { .. } => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::modules::restart::MAX_FAILED_READIES;

    fn id() -> ModuleId {
        ModuleId::try_new("com.example.test".to_owned()).unwrap()
    }

    fn timings() -> ModuleTimings {
        ModuleTimings {
            first_restart: Duration::from_secs(1),
            ..ModuleTimings::default()
        }
    }

    fn run(generation: u64) -> LiveRun {
        LiveRun {
            handle: RunHandle::detached(),
            generation,
            fingerprint: 7,
            settings: toml::Table::new(),
        }
    }

    fn ended(generation: u64, ready_for: Option<Duration>) -> LifeEvent {
        LifeEvent::Ended {
            generation,
            end: RunEnd {
                ready_for,
                error: Some("exited with status 3".into()),
            },
        }
    }

    /// A module brought to `stage` through real transitions at `now`, its
    /// run, where it has one, of generation 1.
    fn module_in(stage: &str, now: Instant) -> Module {
        let mut module = Module::new(timings(), 0);
        let mut step = |event| module.step(&id(), event, now).unwrap();
        match stage {
            "idle" => {}
            "starting" => step(LifeEvent::Started(run(1))),
            "running" => {
                step(LifeEvent::Started(run(1)));
                step(LifeEvent::Ready { generation: 1 });
            }
            "stopping" => {
                step(LifeEvent::Started(run(1)));
                step(LifeEvent::Stop);
            }
            "waiting" => {
                step(LifeEvent::Started(run(1)));
                step(ended(1, None));
            }
            "failed" => {
                // Each start waits out the delay the last end set.
                let mut at = now;
                for generation in 1..=u64::from(MAX_FAILED_READIES) {
                    module
                        .step(&id(), LifeEvent::Started(run(generation)), at)
                        .unwrap();
                    module.step(&id(), ended(generation, None), at).unwrap();
                    at = module.restart_at().unwrap_or(at);
                }
            }
            _ => unreachable!("no stage {stage}"),
        }
        assert_eq!(module.life.name(), stage, "set up {stage}");
        module
    }

    /// An event's name, how to make it, and the stages that refuse it.
    type Refusal = (&'static str, fn() -> LifeEvent, &'static [&'static str]);

    /// Every edge an event has no business taking is refused and leaves the
    /// stage as it was: a second start, `ready` or an end without a run or
    /// from another run, `ready` twice, a stop with nothing to stop, and a
    /// start before a restart is due or after a failure.
    #[test]
    fn every_edge_outside_the_lifecycle_is_refused() {
        let now = Instant::now();
        let refusals: [Refusal; 5] = [
            (
                "start",
                || LifeEvent::Started(run(2)),
                &["starting", "running", "stopping", "waiting", "failed"],
            ),
            (
                "ready",
                || LifeEvent::Ready { generation: 1 },
                &["idle", "running", "waiting", "failed"],
            ),
            (
                "ready",
                || LifeEvent::Ready { generation: 2 },
                &[
                    "idle", "starting", "running", "stopping", "waiting", "failed",
                ],
            ),
            (
                "end",
                || ended(2, None),
                &[
                    "idle", "starting", "running", "stopping", "waiting", "failed",
                ],
            ),
            ("stop", || LifeEvent::Stop, &["idle", "waiting", "failed"]),
        ];
        for (event, make, stages) in refusals {
            for &stage in stages {
                let mut module = module_in(stage, now);
                assert_eq!(
                    module.step(&id(), make(), now),
                    Err(Refused { stage, event }),
                    "{event} while {stage}"
                );
                assert_eq!(module.life.name(), stage, "{event} left {stage} alone");
            }
        }
        for stage in ["idle", "waiting", "failed"] {
            let mut module = module_in(stage, now);
            assert_eq!(
                module.step(&id(), ended(1, None), now),
                Err(Refused {
                    stage,
                    event: "end"
                }),
                "an end with no run while {stage}"
            );
        }
    }

    /// The edges that exist, and what each stage carries across them.
    #[test]
    fn a_module_moves_through_its_stages_and_keeps_its_error_until_ready() {
        let now = Instant::now();
        let mut module = module_in("waiting", now);
        assert_eq!(module.restart_at(), Some(now + Duration::from_secs(1)));
        assert_eq!(module.error(), Some("exited with status 3"));
        assert!(!module.due(now), "not before its restart time");
        let later = now + Duration::from_secs(1);
        assert!(module.due(later));

        module
            .step(&id(), LifeEvent::Started(run(2)), later)
            .unwrap();
        assert_eq!(
            module.error(),
            Some("exited with status 3"),
            "a starting run keeps the last error"
        );
        assert!(module.running().is_none(), "no presses before ready");
        module
            .step(&id(), LifeEvent::Ready { generation: 2 }, later)
            .unwrap();
        assert_eq!(module.error(), None, "ready clears it");
        assert!(module.running().is_some());

        module.step(&id(), LifeEvent::Stop, later).unwrap();
        assert!(module.stopping());
        assert!(!module.takes_faces(2), "a stopping run draws nothing");
        module.step(&id(), LifeEvent::Stop, later).unwrap();
        module.step(&id(), ended(2, None), later).unwrap();
        assert_eq!(
            module.life.name(),
            "idle",
            "a run the host ended is no failure"
        );
        assert_eq!(module.error(), None);
    }

    /// A failure waits for a toggle or a new folder; a pending restart is
    /// forgotten on a toggle too.
    #[test]
    fn a_failure_clears_on_a_toggle_or_a_new_folder() {
        let now = Instant::now();
        let mut module = module_in("failed", now);
        assert!(!module.due(now));
        module
            .step(&id(), LifeEvent::Folder { fingerprint: 7 }, now)
            .unwrap();
        assert_eq!(module.life.name(), "failed", "the same folder stays failed");
        module
            .step(&id(), LifeEvent::Folder { fingerprint: 8 }, now)
            .unwrap();
        assert_eq!(module.life.name(), "idle");
        assert_eq!(
            module.error(),
            Some("exited with status 3"),
            "the failure stays shown until a run gets ready"
        );

        for stage in ["failed", "waiting"] {
            let mut module = module_in(stage, now);
            module.step(&id(), LifeEvent::TurnedOff, now).unwrap();
            assert_eq!(module.life.name(), "idle", "a toggle clears {stage}");
            assert_eq!(module.error(), None);
        }
        let mut module = module_in("running", now);
        module.step(&id(), LifeEvent::TurnedOff, now).unwrap();
        assert_eq!(module.life.name(), "running", "a toggle leaves a run alone");
    }
}
