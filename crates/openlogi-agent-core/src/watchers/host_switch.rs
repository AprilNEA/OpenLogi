//! Keep configured keyboard → pointing-device host-switch links armed.

use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use openlogi_hid::{ChannelPool, DeviceRoute, run_host_switch_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::receiver_access::ReceiverAccess;

/// One resolved link. Config keys are converted to live routes by the
/// orchestrator so the transport watcher never needs to understand inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchLink {
    /// Keyboard whose host switch keys initiate the transition.
    pub keyboard: DeviceRoute,
    /// Pointing devices that follow the keyboard.
    pub targets: Vec<DeviceRoute>,
}

/// Shared resolved links, refreshed with config and inventory.
pub type HostSwitchLinks = Arc<RwLock<Vec<HostSwitchLink>>>;

/// Spawn the host switch session manager.
pub fn spawn(links: HostSwitchLinks, channel_pool: ChannelPool, receiver_access: ReceiverAccess) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "host switch watcher: could not build tokio runtime");
                return;
            }
        };
        runtime.block_on(manage(links, channel_pool, receiver_access));
    });
}

async fn manage(
    links: HostSwitchLinks,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
) {
    let mut current = Vec::new();
    let mut sessions = Vec::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<u64>();
    let mut generation = 0_u64;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let wanted = if receiver_access.exclusive_requested() {
                    Vec::new()
                } else {
                    links.read().map_or_else(|_| Vec::new(), |guard| guard.clone())
                };
                if wanted == current {
                    continue;
                }
                stop_all(&mut sessions).await;
                current.clone_from(&wanted);
                generation = generation.wrapping_add(1);
                for link in wanted {
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let done = done_tx.clone();
                    let pool = channel_pool.clone();
                    let Some(receiver_lease) = receiver_access.try_acquire_for_session() else {
                        current.clear();
                        break;
                    };
                    let session_generation = generation;
                    let task = tokio::spawn(async move {
                        let _receiver_lease = receiver_lease;
                        let keyboard = link.keyboard.clone();
                        if let Err(error) =
                            run_host_switch_session(link.keyboard, link.targets, stop_rx, pool).await
                        {
                            debug!(%error, route = %keyboard, "host switch session ended");
                        }
                        let _ = done.send(session_generation);
                    });
                    sessions.push(RunningSession {
                        stop: stop_tx,
                        task,
                    });
                }
            }
            Some(done_generation) = done_rx.recv() => {
                // A host switch intentionally disconnects the keyboard. Clear
                // the local snapshot so the next tick attempts to arm it again;
                // this also recovers transient setup/read failures.
                if done_generation == generation && !sessions.is_empty() {
                    stop_all(&mut sessions).await;
                    current.clear();
                }
            }
        }
    }
}

struct RunningSession {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

async fn stop_all(sessions: &mut Vec<RunningSession>) {
    let running = std::mem::take(sessions);
    let mut tasks = Vec::with_capacity(running.len());
    for RunningSession { stop, task } in running {
        let _ = stop.send(());
        tasks.push(task);
    }
    for task in tasks {
        let _ = task.await;
    }
}
