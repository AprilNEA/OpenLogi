//! Thread shell and graceful-stop primitive shared by the HID++ watcher
//! managers.
//!
//! Each manager runs on its own thread and current-thread Tokio runtime, so
//! dropping the process's main runtime does not ask its active sessions to
//! restore firmware diversion. [`WatcherHandle`] starts that thread and gives
//! the process lifecycle one explicit stop request and one acknowledgement
//! that the manager no longer owns a task capable of writing the device.

use std::future::Future;
use std::io;

use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tracing::warn;

/// What a worker thread runs once it owns its runtime.
type WorkerBody = Box<dyn FnOnce(Runtime) + Send>;

/// Whether a watcher manager acknowledged that it no longer owns live
/// firmware-writing tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// The manager completed its requested graceful stop.
    Stopped,
    /// The manager ended through another control-plane channel before it could
    /// perform the requested graceful stop.
    Unclean,
    /// The manager thread ended without sending its acknowledgement.
    CompletionLost,
}

/// How a manager loop returned before its runtime was destroyed.
#[derive(Debug)]
pub(super) enum ManagerCompletion {
    /// The process stop request drove the manager's teardown.
    Graceful,
    /// Another control-plane source closed first.
    Unexpected,
}

impl StopOutcome {
    /// Whether the manager explicitly acknowledged that it stopped.
    #[must_use]
    pub const fn is_stopped(self) -> bool {
        matches!(self, Self::Stopped)
    }
}

/// Process-lifecycle handle for one HID++ watcher manager.
pub struct WatcherHandle {
    stop: oneshot::Sender<()>,
    done: oneshot::Receiver<ManagerCompletion>,
}

impl WatcherHandle {
    /// Run a manager on a worker thread named `name` that owns its
    /// current-thread runtime. `manage` is handed the stop request's receiving
    /// end and returns how the manager loop ended.
    ///
    /// The runtime is dropped before completion is acknowledged: detached
    /// session tasks belong to it, so even an unexpected manager return cannot
    /// leave a late firmware writer behind the process boundary.
    ///
    /// A worker that cannot start owns no such task, but it did not stop
    /// gracefully either, so its handle reports [`StopOutcome::Unclean`].
    #[must_use]
    pub(super) fn spawn<F>(
        name: &'static str,
        manage: impl FnOnce(oneshot::Receiver<()>) -> F + Send + 'static,
    ) -> Self
    where
        F: Future<Output = ManagerCompletion>,
    {
        Self::spawn_with(openlogi_core::worker::spawn, name, manage)
    }

    /// [`Self::spawn`] with the step that starts the worker thread injected.
    fn spawn_with<F>(
        start: impl FnOnce(&'static str, WorkerBody) -> io::Result<()>,
        name: &'static str,
        manage: impl FnOnce(oneshot::Receiver<()>) -> F + Send + 'static,
    ) -> Self
    where
        F: Future<Output = ManagerCompletion>,
    {
        let (stop, stopped) = oneshot::channel();
        let (done_tx, done) = oneshot::channel();
        let started = start(
            name,
            Box::new(move |runtime| {
                let completion = runtime.block_on(manage(stopped));
                drop(runtime);
                let _ = done_tx.send(completion);
            }),
        );
        let Err(error) = started else {
            return Self { stop, done };
        };
        warn!(%error, watcher = name, "could not start the watcher's worker thread");
        let (never_started, done) = oneshot::channel();
        let _ = never_started.send(ManagerCompletion::Unexpected);
        Self { stop, done }
    }

    /// Request an ordered stop and wait for confirmed teardown.
    ///
    /// The process lifecycle owns the deadline policy. It must retain this
    /// future across control-plane events during replacement, and may abandon
    /// it at a deadline only when the process is about to exit.
    pub async fn stop_and_wait(self, watcher: &'static str) -> StopOutcome {
        let _ = self.stop.send(());
        completion(self.done.await, watcher)
    }
}

fn completion(
    result: Result<ManagerCompletion, oneshot::error::RecvError>,
    watcher: &'static str,
) -> StopOutcome {
    match result {
        Ok(ManagerCompletion::Graceful) => StopOutcome::Stopped,
        Ok(ManagerCompletion::Unexpected) => {
            warn!(watcher, "watcher ended before its graceful stop request");
            StopOutcome::Unclean
        }
        Err(error) => {
            warn!(%error, watcher, "watcher ended without acknowledging its completion");
            StopOutcome::CompletionLost
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_spawned_manager_is_asked_to_stop_and_acknowledges() {
        let handle = WatcherHandle::spawn("openlogi-test-watcher", |stop| async move {
            match stop.await {
                Ok(()) => ManagerCompletion::Graceful,
                Err(_) => ManagerCompletion::Unexpected,
            }
        });

        assert_eq!(handle.stop_and_wait("test").await, StopOutcome::Stopped);
    }

    #[tokio::test]
    async fn a_worker_that_never_started_is_not_reported_as_stopped() {
        let handle = WatcherHandle::spawn_with(
            |_name, _body| Err(io::Error::other("no thread for the worker")),
            "openlogi-test-watcher",
            |_stop| async { ManagerCompletion::Graceful },
        );

        assert_eq!(handle.stop_and_wait("test").await, StopOutcome::Unclean);
    }

    #[tokio::test]
    async fn explicit_completion_is_stopped() {
        let (stop_tx, stop_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        done_tx
            .send(ManagerCompletion::Graceful)
            .expect("completion receiver should be open");

        let outcome = WatcherHandle {
            stop: stop_tx,
            done: done_rx,
        }
        .stop_and_wait("test")
        .await;

        assert_eq!(outcome, StopOutcome::Stopped);
        stop_rx.await.expect("stop request should be sent");
    }

    #[tokio::test]
    async fn dropped_completion_is_not_reported_as_stopped() {
        let (stop_tx, _stop_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        drop(done_tx);

        let outcome = WatcherHandle {
            stop: stop_tx,
            done: done_rx,
        }
        .stop_and_wait("test")
        .await;

        assert_eq!(outcome, StopOutcome::CompletionLost);
    }

    #[tokio::test]
    async fn unexpected_manager_return_is_not_reported_as_stopped() {
        let (stop_tx, _stop_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        done_tx
            .send(ManagerCompletion::Unexpected)
            .expect("completion receiver should be open");

        let outcome = WatcherHandle {
            stop: stop_tx,
            done: done_rx,
        }
        .stop_and_wait("test")
        .await;

        assert_eq!(outcome, StopOutcome::Unclean);
    }
}
