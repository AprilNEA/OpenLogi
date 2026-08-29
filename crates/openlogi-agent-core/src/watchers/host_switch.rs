//! Keep configured keyboard → pointing-device host-switch links armed.

use std::thread;
use std::time::Duration;

use openlogi_hid::{
    ChannelPool, DeviceRoute, HostSwitchStopReason, run_host_switch_session, switch_linked_hosts,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess, ReceiverRequestState};

const DEPARTURE_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// One resolved link. Config keys are converted to live routes by the
/// orchestrator so the transport watcher never needs to understand inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchLink {
    /// Keyboard whose host switch keys initiate the transition.
    pub keyboard: DeviceRoute,
    /// Pointing devices that follow the keyboard.
    pub targets: Vec<DeviceRoute>,
}

/// Read-only, lossless, coalescing view of resolved links.
pub type HostSwitchLinks = watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>;

/// Spawn the host switch session manager.
pub fn spawn(links: &HostSwitchLinks, channel_pool: ChannelPool, receiver_access: ReceiverAccess) {
    let links = links.clone();
    let receiver_requests = receiver_access.subscribe_requests();
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
        runtime.block_on(manage(
            links,
            channel_pool,
            receiver_access,
            receiver_requests,
        ));
    });
}

struct HostSwitchManagerState {
    sessions: Vec<RunningSession>,
    next_generation: u64,
    restart_after: Vec<(HostSwitchLink, Instant)>,
}

impl HostSwitchManagerState {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            next_generation: 0,
            restart_after: Vec::new(),
        }
    }

    fn deadline(&self, requests: ReceiverRequestState) -> Option<Instant> {
        if requests.any() {
            return None;
        }
        self.restart_after
            .iter()
            .map(|(_, deadline)| *deadline)
            .min()
    }

    async fn reconcile(
        &mut self,
        requests: ReceiverRequestState,
        published: &std::sync::Arc<Vec<HostSwitchLink>>,
        receiver_access: &ReceiverAccess,
        channel_pool: &ChannelPool,
        done: &mpsc::UnboundedSender<SessionCompletion>,
    ) {
        let now = Instant::now();
        let wanted = if requests.any() {
            &[][..]
        } else {
            published.as_slice()
        };
        stop_unwanted(&mut self.sessions, wanted).await;
        self.restart_after
            .retain(|(link, _)| published.contains(link));
        for link in wanted {
            if self.sessions.iter().any(|session| session.link == *link)
                || self
                    .restart_after
                    .iter()
                    .any(|(delayed, deadline)| delayed == link && *deadline > now)
            {
                continue;
            }
            self.restart_after.retain(|(delayed, _)| delayed != link);
            let Some(receiver_lease) = receiver_access.try_acquire_for_session() else {
                self.restart_after.push((link.clone(), now + RETRY_DELAY));
                break;
            };
            self.next_generation = self.next_generation.wrapping_add(1);
            self.sessions.push(spawn_session(
                link.clone(),
                self.next_generation,
                receiver_lease,
                channel_pool.clone(),
                done.clone(),
            ));
        }
    }
}

async fn manage(
    mut links: watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    mut receiver_requests: watch::Receiver<ReceiverRequestState>,
) {
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<SessionCompletion>();
    let mut state = HostSwitchManagerState::new();
    let mut reconcile = true;

    loop {
        if reconcile {
            reconcile = false;
            let requests = *receiver_requests.borrow_and_update();
            let published = std::sync::Arc::clone(&links.borrow_and_update());
            state
                .reconcile(
                    requests,
                    &published,
                    &receiver_access,
                    &channel_pool,
                    &done_tx,
                )
                .await;
        }

        let requests = *receiver_requests.borrow();
        let deadline = state.deadline(requests);
        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            reconcile = true;
            continue;
        }

        tokio::select! {
            Some(completion) = done_rx.recv() => {
                if let Some(index) = state.sessions
                    .iter()
                    .position(|session| session.generation == completion.generation)
                {
                    let completed = state.sessions.remove(index);
                    let _ = completed.task.await;
                    if let Some((link, host)) = completion.request {
                        stop_all(&mut state.sessions, HostSwitchStopReason::Graceful).await;
                        run_transition(&mut links, &channel_pool, &receiver_access, link, host).await;
                    } else {
                        state.restart_after.push((completed.link, Instant::now() + RETRY_DELAY));
                    }
                    reconcile = true;
                }
            }
            result = links.changed() => {
                if result.is_err() {
                    return;
                }
                reconcile = true;
            }
            result = receiver_requests.changed() => {
                if result.is_err() {
                    return;
                }
                reconcile = true;
            }
            () = wait_for_deadline(deadline) => {
                reconcile = true;
            }
        }
    }
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn spawn_session(
    link: HostSwitchLink,
    generation: u64,
    receiver_lease: crate::receiver_access::SessionReceiverLease,
    pool: ChannelPool,
    done: mpsc::UnboundedSender<SessionCompletion>,
) -> RunningSession {
    let (stop, stop_rx) = oneshot::channel();
    let session_link = link.clone();
    let task = tokio::spawn(async move {
        let _receiver_lease = receiver_lease;
        let keyboard = session_link.keyboard.clone();
        let request =
            match run_host_switch_session(session_link.keyboard.clone(), stop_rx, pool).await {
                Ok(host) => host.map(|host| (session_link, host)),
                Err(error) => {
                    debug!(%error, route = %keyboard, "host switch session ended");
                    None
                }
            };
        let _ = done.send(SessionCompletion {
            generation,
            request,
        });
    });
    RunningSession {
        link,
        generation,
        stop,
        task,
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
    links: &mut watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>,
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

async fn wait_for_departure(
    links: &mut watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>,
    keyboard: &DeviceRoute,
) {
    let deadline = tokio::time::sleep(DEPARTURE_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        let departed = !links
            .borrow_and_update()
            .iter()
            .any(|link| link.keyboard == *keyboard);
        if departed {
            return;
        }
        tokio::select! {
            result = links.changed() => {
                if result.is_err() {
                    return;
                }
            }
            () = &mut deadline => {
                warn!(route = %keyboard, "host transition departure was not observed");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(slot: u8) -> DeviceRoute {
        DeviceRoute::Bolt {
            receiver_uid: "cafe".to_owned(),
            slot,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn departure_publication_finishes_wait_without_advancing_time() {
        let keyboard = route(1);
        let link = HostSwitchLink {
            keyboard: keyboard.clone(),
            targets: vec![route(2)],
        };
        let (links, mut published) = watch::channel(std::sync::Arc::new(vec![link]));
        let started = Instant::now();
        let waiting = tokio::spawn(async move {
            wait_for_departure(&mut published, &keyboard).await;
            Instant::now()
        });
        tokio::task::yield_now().await;

        links.send_replace(std::sync::Arc::new(Vec::new()));
        tokio::task::yield_now().await;

        assert_eq!(
            waiting.await.expect("departure waiter should finish"),
            started,
            "the link publication should reconcile departure immediately"
        );
    }
}
