//! Pure lifecycle state for the Windows hook worker.

#[cfg(target_os = "windows")]
use std::sync::{Mutex, PoisonError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerPhase {
    Starting,
    Running,
    StopRequested,
    Stopped,
    Failed,
}

impl WorkerPhase {
    pub(super) const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerEvent {
    Started,
    StopRequested,
    MessageLoopQuit,
    MessageLoopFailed,
}

pub(super) const fn worker_transition(phase: WorkerPhase, event: WorkerEvent) -> WorkerPhase {
    match (phase, event) {
        (WorkerPhase::Starting, WorkerEvent::Started) => WorkerPhase::Running,
        (WorkerPhase::Running, WorkerEvent::StopRequested) => WorkerPhase::StopRequested,
        (WorkerPhase::Running | WorkerPhase::StopRequested, WorkerEvent::MessageLoopQuit) => {
            WorkerPhase::Stopped
        }
        (WorkerPhase::Running | WorkerPhase::StopRequested, WorkerEvent::MessageLoopFailed) => {
            WorkerPhase::Failed
        }
        _ => phase,
    }
}

#[cfg(target_os = "windows")]
pub(super) struct WorkerStatus {
    phase: Mutex<WorkerPhase>,
}

#[cfg(target_os = "windows")]
impl WorkerStatus {
    pub(super) const fn new() -> Self {
        Self {
            phase: Mutex::new(WorkerPhase::Starting),
        }
    }

    pub(super) fn phase(&self) -> WorkerPhase {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn transition(&self, event: WorkerEvent) -> WorkerPhase {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        let previous = *phase;
        *phase = worker_transition(previous, event);
        previous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_stop_has_an_explicit_terminal_path() {
        let running = worker_transition(WorkerPhase::Starting, WorkerEvent::Started);
        assert_eq!(running, WorkerPhase::Running);
        assert!(running.is_running());

        let stopping = worker_transition(running, WorkerEvent::StopRequested);
        assert_eq!(stopping, WorkerPhase::StopRequested);
        assert!(!stopping.is_running());

        let stopped = worker_transition(stopping, WorkerEvent::MessageLoopQuit);
        assert_eq!(stopped, WorkerPhase::Stopped);
        assert!(!stopped.is_running());
    }

    #[test]
    fn message_loop_error_is_terminal_before_and_during_stop() {
        for phase in [WorkerPhase::Running, WorkerPhase::StopRequested] {
            let failed = worker_transition(phase, WorkerEvent::MessageLoopFailed);
            assert_eq!(failed, WorkerPhase::Failed);
            assert!(!failed.is_running());
            assert_eq!(
                worker_transition(failed, WorkerEvent::StopRequested),
                WorkerPhase::Failed,
                "teardown must not revive a failed worker"
            );
        }
    }
}
