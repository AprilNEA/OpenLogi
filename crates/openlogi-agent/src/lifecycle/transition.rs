//! Ownership of the watcher fleet during process replacement and terminal exit.

use std::path::PathBuf;
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::BoxFuture;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::startup::HidppWatcherHandles;

const STOP_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Replacement {
    pub(super) path: PathBuf,
    pub(super) retry: oneshot::Sender<()>,
}

pub(super) enum WatcherFleet {
    Inactive,
    Running(HidppWatcherHandles),
    Replacing {
        request: Replacement,
        teardown: BoxFuture<'static, bool>,
    },
}

impl WatcherFleet {
    /// Begin teardown without blocking the lifecycle's other event sources.
    pub(super) fn begin_replacement(&mut self, request: Replacement) {
        match std::mem::replace(self, Self::Inactive) {
            Self::Running(handles) => {
                info!(path = %request.path.display(), "executable changed — draining HID++ sessions before replacement");
                *self = Self::Replacing {
                    request,
                    teardown: handles.stop_and_wait().boxed(),
                };
            }
            state => {
                // The binary watcher normally has only one outstanding
                // request. Do not discard an existing drain if one overlaps.
                *self = state;
                let _ = request.retry.send(());
            }
        }
    }

    /// Poll the owned teardown in the main select loop. Losing a select race
    /// drops only this borrow, not the managers' completion receivers.
    pub(super) async fn replacement_ready(&mut self) -> (Replacement, bool) {
        let Self::Replacing { teardown, .. } = self else {
            return std::future::pending().await;
        };
        let stopped = teardown.await;
        match std::mem::replace(self, Self::Inactive) {
            Self::Replacing { request, .. } => (request, stopped),
            _ => unreachable!("exclusive borrow preserves the replacement until completion"),
        }
    }

    /// A terminal exit keeps polling the same drain, but no longer waits
    /// indefinitely for disconnected hardware. Never use this for replacement.
    pub(super) async fn stop_for_exit(self) {
        let teardown = match self {
            Self::Inactive => return,
            Self::Running(handles) => handles.stop_and_wait().boxed(),
            Self::Replacing { teardown, .. } => teardown,
        };
        if tokio::time::timeout(STOP_TIMEOUT, teardown).await.is_err() {
            warn!(timeout = ?STOP_TIMEOUT, "watcher teardown exceeded the process-exit deadline");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shutdown::{ShutdownRequest, request_channel};

    fn replacing() -> (WatcherFleet, oneshot::Sender<bool>, oneshot::Receiver<()>) {
        let (completed, completion) = oneshot::channel();
        let (retry, retried) = oneshot::channel();
        let fleet = WatcherFleet::Replacing {
            request: Replacement {
                path: PathBuf::from("replacement-agent"),
                retry,
            },
            teardown: async move { completion.await.expect("test controls completion") }.boxed(),
        };
        (fleet, completed, retried)
    }

    #[tokio::test(start_paused = true)]
    async fn pending_replacement_keeps_accepting_exit_requests_and_bounds_the_same_drain() {
        let (mut fleet, completed, _retried) = replacing();
        let (requests, mut incoming) = request_channel();
        let quit = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            requests.send(ShutdownRequest::Uninstalled).unwrap();
        });

        tokio::select! {
            _ = fleet.replacement_ready() => panic!("replacement cannot proceed without teardown"),
            request = incoming.recv() => assert!(matches!(request, Some(ShutdownRequest::Uninstalled))),
        }
        quit.await.unwrap();
        assert!(
            !completed.is_closed(),
            "accepting Quit must retain the existing cleanup"
        );

        let started = tokio::time::Instant::now();
        fleet.stop_for_exit().await;
        assert_eq!(started.elapsed(), STOP_TIMEOUT);
        assert!(
            completed.is_closed(),
            "terminal timeout may abandon completion before exit"
        );
    }

    #[tokio::test]
    async fn losing_select_polls_retains_completion_and_its_failure_status() {
        for stopped in [false, true] {
            let (mut fleet, completed, mut retried) = replacing();
            for _ in 0..3 {
                assert!(fleet.replacement_ready().now_or_never().is_none());
            }
            completed.send(stopped).unwrap();
            let (request, actual) = fleet.replacement_ready().await;
            assert_eq!(actual, stopped);
            assert_eq!(request.path, PathBuf::from("replacement-agent"));
            assert!(matches!(fleet, WatcherFleet::Inactive));
            assert!(matches!(
                retried.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            request.retry.send(()).unwrap();
            retried.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_exit_accepts_existing_cleanup_before_its_deadline() {
        let (fleet, completed, _retried) = replacing();
        let started = tokio::time::Instant::now();
        let completion = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            completed.send(true).unwrap();
        });
        fleet.stop_for_exit().await;
        completion.await.unwrap();
        assert_eq!(started.elapsed(), Duration::from_millis(1200));
    }
}
