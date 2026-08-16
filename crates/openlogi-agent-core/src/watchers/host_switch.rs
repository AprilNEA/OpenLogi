//! Keep configured keyboard → pointing-device host-switch links armed.

use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_hid::{
    ChannelPool, DeviceRoute, HostSwitchStopReason, run_host_switch_session, switch_linked_hosts,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess};

const DEPARTURE_TIMEOUT: Duration = Duration::from_secs(10);
const DEPARTURE_POLL: Duration = Duration::from_millis(100);

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
    let mut sessions = Vec::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<SessionCompletion>();
    let mut next_generation = 0_u64;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let wanted = if receiver_access.exclusive_requested() {
                    Vec::new()
                } else {
                    links.read().map_or_else(|_| Vec::new(), |guard| guard.clone())
                };
                stop_unwanted(&mut sessions, &wanted).await;
                for link in wanted {
                    if sessions.iter().any(|session| session.link == link) {
                        continue;
                    }
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let done = done_tx.clone();
                    let pool = channel_pool.clone();
                    let Some(receiver_lease) = receiver_access.try_acquire_for_session() else {
                        break;
                    };
                    next_generation = next_generation.wrapping_add(1);
                    let session_generation = next_generation;
                    let session_link = link.clone();
                    let task = tokio::spawn(async move {
                        let _receiver_lease = receiver_lease;
                        let keyboard = link.keyboard.clone();
                        let request = match run_host_switch_session(
                            link.keyboard.clone(),
                            stop_rx,
                            pool,
                        )
                        .await
                        {
                            Ok(host) => host.map(|host| (link, host)),
                            Err(error) => {
                                debug!(%error, route = %keyboard, "host switch session ended");
                                None
                            }
                        };
                        let _ = done.send(SessionCompletion {
                            generation: session_generation,
                            request,
                        });
                    });
                    sessions.push(RunningSession {
                        link: session_link,
                        generation: session_generation,
                        stop: stop_tx,
                        task,
                    });
                }
            }
            Some(completion) = done_rx.recv() => {
                if let Some(index) = sessions
                    .iter()
                    .position(|session| session.generation == completion.generation)
                {
                    let completed = sessions.remove(index);
                    let _ = completed.task.await;
                    if let Some((link, host)) = completion.request {
                        stop_all(&mut sessions, HostSwitchStopReason::Graceful).await;
                        run_transition(&links, &channel_pool, &receiver_access, link, host).await;
                    }
                }
            }
        }
    }
}

struct RunningSession {
    link: HostSwitchLink,
    generation: u64,
    stop: oneshot::Sender<HostSwitchStopReason>,
    task: tokio::task::JoinHandle<()>,
}

struct SessionCompletion {
    generation: u64,
    request: Option<(HostSwitchLink, u8)>,
}

async fn stop_all(sessions: &mut Vec<RunningSession>, reason: HostSwitchStopReason) {
    let running = std::mem::take(sessions);
    let mut tasks = Vec::with_capacity(running.len());
    for RunningSession { stop, task, .. } in running {
        let _ = stop.send(reason);
        tasks.push(task);
    }
    for task in tasks {
        let _ = task.await;
    }
}

async fn stop_unwanted(sessions: &mut Vec<RunningSession>, wanted: &[HostSwitchLink]) {
    let mut index = 0;
    while index < sessions.len() {
        if wanted.contains(&sessions[index].link) {
            index += 1;
            continue;
        }
        let RunningSession { stop, task, .. } = sessions.remove(index);
        let _ = stop.send(HostSwitchStopReason::Graceful);
        let _ = task.await;
    }
}

async fn run_transition(
    links: &HostSwitchLinks,
    channel_pool: &ChannelPool,
    receiver_access: &ReceiverAccess,
    link: HostSwitchLink,
    host: u8,
) {
    let _lease = receiver_access
        .acquire_exclusive(ExclusiveAccessReason::HostTransition)
        .await;
    match switch_linked_hosts(&link.keyboard, &link.targets, host, channel_pool).await {
        Ok(true) => wait_for_departure(links, &link.keyboard).await,
        Ok(false) => {}
        Err(error) => {
            debug!(%error, route = %link.keyboard, host, "keyboard host switch failed");
        }
    }
}

async fn wait_for_departure(links: &HostSwitchLinks, keyboard: &DeviceRoute) {
    let deadline = Instant::now() + DEPARTURE_TIMEOUT;
    while Instant::now() < deadline {
        let departed = links.read().map_or(true, |current| {
            !current.iter().any(|link| link.keyboard == *keyboard)
        });
        if departed {
            return;
        }
        tokio::time::sleep(DEPARTURE_POLL).await;
    }
    warn!(route = %keyboard, "host transition departure was not observed");
}
