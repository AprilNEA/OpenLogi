//! Pure lifecycle state for the Windows hook worker.

use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::sync::{Mutex, PoisonError};

use crate::ForegroundApp;

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

/// Coalesces a burst of native notifications until their queued snapshot has
/// been delivered by the owning message pump.
pub(super) struct NotificationLatch {
    pending: AtomicBool,
}

impl NotificationLatch {
    pub(super) const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    /// Claim responsibility for queueing the next delivery.
    pub(super) fn claim(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    /// Allow a later native notification to queue another delivery.
    pub(super) fn delivered(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

/// The last semantic foreground snapshot delivered by the Windows observer.
pub(super) struct ForegroundChanges {
    published: PublishedForeground,
}

enum PublishedForeground {
    NotYet,
    Value(Option<ForegroundApp>),
}

impl Default for ForegroundChanges {
    fn default() -> Self {
        Self {
            published: PublishedForeground::NotYet,
        }
    }
}

impl ForegroundChanges {
    pub(super) fn observe(&mut self, current: Option<&ForegroundApp>) -> bool {
        if matches!(
            &self.published,
            PublishedForeground::Value(published) if published.as_ref() == current
        ) {
            return false;
        }
        self.published = PublishedForeground::Value(current.cloned());
        true
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

    fn app(id: &str) -> ForegroundApp {
        ForegroundApp::unnamed(id.to_owned())
    }

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

    #[test]
    fn notification_bursts_queue_one_delivery_until_observed() {
        let latch = NotificationLatch::new();

        assert!(latch.claim());
        assert!(!latch.claim());
        assert!(!latch.claim());
        latch.delivered();
        assert!(latch.claim());
    }

    #[test]
    fn foreground_changes_publish_the_initial_snapshot_and_semantic_changes() {
        let mut changes = ForegroundChanges::default();

        assert!(changes.observe(None));
        assert!(!changes.observe(None));
        let one = app("c:\\apps\\one.exe");
        assert!(changes.observe(Some(&one)));
        assert!(!changes.observe(Some(&one)));
        let two = app("c:\\apps\\two.exe");
        assert!(changes.observe(Some(&two)));
        assert!(changes.observe(None));
    }
}
