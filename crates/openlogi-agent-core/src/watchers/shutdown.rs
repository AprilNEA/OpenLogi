//! Graceful-stop primitive shared by the HID++ watcher managers.
//!
//! Each manager runs on its own thread and current-thread Tokio runtime, so
//! dropping the process's main runtime does not ask its active sessions to
//! restore firmware diversion. [`WatcherHandle`] gives the process lifecycle
//! one explicit stop request and one acknowledgement that the manager no
//! longer owns a task capable of writing the device.

use std::time::Duration;

use tokio::sync::oneshot;
use tracing::warn;

/// How long a terminal process exit waits for one watcher manager to stop.
///
/// HID++ restore writes normally take tens of milliseconds. A bounded exit
/// prevents one disconnected device from keeping an uninstalled or quitting
/// process alive indefinitely; process replacement uses the confirmed form
/// because it must not hand unresolved firmware ownership to a new image.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether a watcher manager acknowledged that it no longer owns live
/// firmware-writing tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// The manager completed its requested graceful stop.
    Stopped,
    /// A terminal exit reached its watcher-shutdown deadline.
    TimedOut,
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
    stop: Option<oneshot::Sender<()>>,
    done: oneshot::Receiver<ManagerCompletion>,
}

impl WatcherHandle {
    /// Pair the manager's stop request with its completion acknowledgement.
    #[must_use]
    pub(super) fn new(
        stop: oneshot::Sender<()>,
        done: oneshot::Receiver<ManagerCompletion>,
    ) -> Self {
        Self {
            stop: Some(stop),
            done,
        }
    }

    /// Request an ordered stop and wait up to the terminal-exit deadline.
    pub async fn stop_and_wait(mut self, watcher: &'static str) -> StopOutcome {
        self.request_stop();
        if let Ok(result) = tokio::time::timeout(STOP_TIMEOUT, &mut self.done).await {
            completion(result, watcher)
        } else {
            warn!(
                watcher,
                timeout = ?STOP_TIMEOUT,
                "watcher did not stop before process-exit deadline"
            );
            StopOutcome::TimedOut
        }
    }

    /// Request an ordered stop and continue waiting after the diagnostic
    /// deadline until the manager is known to have ended.
    ///
    /// Process replacement uses this form on every platform: firmware state
    /// outlives a process image, so replacement must not discard unresolved
    /// restore ownership merely because the terminal-exit deadline elapsed.
    pub async fn stop_and_wait_confirmed(mut self, watcher: &'static str) -> StopOutcome {
        self.request_stop();
        if let Ok(result) = tokio::time::timeout(STOP_TIMEOUT, &mut self.done).await {
            completion(result, watcher)
        } else {
            tracing::info!(
                watcher,
                timeout = ?STOP_TIMEOUT,
                "watcher shutdown is slow — continuing to wait before process replacement"
            );
            completion(self.done.await, watcher)
        }
    }

    fn request_stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
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
    async fn explicit_completion_is_stopped() {
        let (stop_tx, stop_rx) = oneshot::channel();
        let (done_tx, done_rx) = oneshot::channel();
        done_tx
            .send(ManagerCompletion::Graceful)
            .expect("completion receiver should be open");

        let outcome = WatcherHandle::new(stop_tx, done_rx)
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

        let outcome = WatcherHandle::new(stop_tx, done_rx)
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

        let outcome = WatcherHandle::new(stop_tx, done_rx)
            .stop_and_wait("test")
            .await;

        assert_eq!(outcome, StopOutcome::Unclean);
    }
}
