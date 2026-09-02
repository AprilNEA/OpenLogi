//! Keep configured keyboard → linked-device host-switch relationships armed.

use std::thread;
use std::time::Duration;

use openlogi_hid::{
    ChannelPool, DeviceIoGate, DeviceRoute, HostSwitchCaptureMode, HostSwitchError,
    HostSwitchRequest, HostSwitchStopReason, KeyboardHostTransition, run_host_switch_session,
    switch_linked_hosts,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess, ReceiverRequestState};

const DEPARTURE_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct TransitionTimeouts {
    commanded_departure: Duration,
}

const TRANSITION_TIMEOUTS: TransitionTimeouts = TransitionTimeouts {
    commanded_departure: DEPARTURE_TIMEOUT,
};

/// One resolved link. Config keys are converted to live routes by the
/// orchestrator so the transport watcher never needs to understand inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchLink {
    /// Stable physical configuration identity of the initiating keyboard.
    pub keyboard_key: String,
    /// Keyboard whose host switch keys initiate the transition.
    pub keyboard: DeviceRoute,
    /// Devices that follow the keyboard.
    pub targets: Vec<DeviceRoute>,
}

/// Read-only, lossless, coalescing view of resolved links.
pub type HostSwitchLinks = watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>;
/// Read-only physical routes from the latest successful inventory snapshot.
/// This stays independent of host-switch configuration so editing a link
/// cannot masquerade as a keyboard departure.
pub type HostSwitchInventory = watch::Receiver<std::sync::Arc<Vec<DeviceRoute>>>;

/// Spawn the host switch session manager.
pub fn spawn(
    links: &HostSwitchLinks,
    inventory: &HostSwitchInventory,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
) {
    let links = links.clone();
    let inventory = inventory.clone();
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
            inventory,
            channel_pool,
            receiver_access,
            receiver_requests,
            device_io,
        ));
    });
}

struct HostSwitchManagerState {
    sessions: Vec<RunningSession>,
    next_generation: u64,
    restart_after: Vec<(HostSwitchLink, Instant)>,
    announcement_keyboards: Vec<String>,
}

impl HostSwitchManagerState {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            next_generation: 0,
            restart_after: Vec::new(),
            announcement_keyboards: Vec::new(),
        }
    }

    fn deadline(&self, requests: ReceiverRequestState, device_io_allowed: bool) -> Option<Instant> {
        if requests.any() || !device_io_allowed {
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
        device_io: &DeviceIoGate,
    ) {
        // Keep armed listeners passive while sleeping. A reconcile to an
        // empty/changed link set would restore firmware controls and a retry
        // would reopen the HID transport during DarkWake.
        if !device_io.allows_io() {
            return;
        }
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
            let capture_mode = capture_mode_for(&self.announcement_keyboards, &link.keyboard_key);
            self.sessions.push(spawn_session(
                link.clone(),
                self.next_generation,
                receiver_lease,
                channel_pool.clone(),
                capture_mode,
                done.clone(),
                device_io.clone(),
            ));
        }
    }
}

async fn manage(
    mut links: HostSwitchLinks,
    mut inventory: HostSwitchInventory,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    mut receiver_requests: watch::Receiver<ReceiverRequestState>,
    mut device_io: DeviceIoGate,
) {
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<SessionCompletion>();
    let mut state = HostSwitchManagerState::new();
    let mut reconcile = true;

    loop {
        if reconcile {
            reconcile = false;
            let device_io_allowed = device_io.allows_io();
            if device_io_allowed {
                let requests = *receiver_requests.borrow_and_update();
                let published = std::sync::Arc::clone(&links.borrow_and_update());
                state
                    .reconcile(
                        requests,
                        &published,
                        &receiver_access,
                        &channel_pool,
                        &done_tx,
                        &device_io,
                    )
                    .await;
            }
        }

        let requests = *receiver_requests.borrow();
        let deadline = state.deadline(requests, device_io.allows_io());
        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            reconcile = true;
            continue;
        }

        tokio::select! {
            Some(completion) = done_rx.recv() => {
                if let Some(accepted) = accept_completion(&mut state.sessions, completion) {
                    let AcceptedCompletion { completed, request } = accepted;
                    let _ = completed.task.await;
                    if let Some((link, request)) = request {
                        if request.keyboard_transition.announcement_observed() {
                            remember_announcement_keyboard(
                                &mut state.announcement_keyboards,
                                link.keyboard_key.clone(),
                            );
                        }
                        debug!(
                            generation = completed.generation,
                            route = %link.keyboard,
                            host = request.host,
                            targets = link.targets.len(),
                            "starting linked transition"
                        );
                        if !device_io.wait_until_allowed().await {
                            return;
                        }
                        stop_all(&mut state.sessions, HostSwitchStopReason::Graceful).await;
                        run_transition(
                            &mut inventory,
                            &channel_pool,
                            &receiver_access,
                            link,
                            request,
                        )
                        .await;
                    } else if device_io.allows_io() {
                        state
                            .restart_after
                            .push((completed.link, Instant::now() + RETRY_DELAY));
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
            allowed = device_io.changed() => match allowed {
                Some(true) => reconcile = true,
                Some(false) => {}
                None => return,
            },
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
    capture_mode: HostSwitchCaptureMode,
    done: mpsc::UnboundedSender<SessionCompletion>,
    device_io: DeviceIoGate,
) -> RunningSession {
    let (stop, stop_rx) = oneshot::channel();
    let session_link = link.clone();
    let task = tokio::spawn(async move {
        let _receiver_lease = receiver_lease;
        let keyboard = session_link.keyboard.clone();
        let request = match run_host_switch_session(
            session_link.keyboard.clone(),
            stop_rx,
            pool,
            capture_mode,
            device_io,
        )
        .await
        {
            Ok(request) => request.map(|request| (session_link, request)),
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
    request: Option<(HostSwitchLink, HostSwitchRequest)>,
}

struct AcceptedCompletion {
    completed: RunningSession,
    request: Option<(HostSwitchLink, HostSwitchRequest)>,
}

/// Accept a completion only while its exact session generation is still live.
///
/// A stopped session can finish after a replacement is armed. Discarding that
/// obsolete request here prevents its host selection from moving the current
/// session's linked devices.
fn accept_completion(
    sessions: &mut Vec<RunningSession>,
    completion: SessionCompletion,
) -> Option<AcceptedCompletion> {
    let index = sessions
        .iter()
        .position(|session| session.generation == completion.generation)?;
    Some(AcceptedCompletion {
        completed: sessions.remove(index),
        request: completion.request,
    })
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
    inventory: &mut HostSwitchInventory,
    channel_pool: &ChannelPool,
    receiver_access: &ReceiverAccess,
    link: HostSwitchLink,
    request: HostSwitchRequest,
) {
    run_transition_with(
        inventory,
        channel_pool,
        receiver_access,
        link,
        request,
        &DeviceTransitionExecutor,
        TRANSITION_TIMEOUTS,
    )
    .await;
}

trait HostTransitionExecutor {
    async fn switch(
        &self,
        keyboard: &DeviceRoute,
        targets: &[DeviceRoute],
        host: u8,
        keyboard_transition: KeyboardHostTransition,
        channel_pool: &ChannelPool,
    ) -> Result<bool, HostSwitchError>;
}

struct DeviceTransitionExecutor;

impl HostTransitionExecutor for DeviceTransitionExecutor {
    async fn switch(
        &self,
        keyboard: &DeviceRoute,
        targets: &[DeviceRoute],
        host: u8,
        keyboard_transition: KeyboardHostTransition,
        channel_pool: &ChannelPool,
    ) -> Result<bool, HostSwitchError> {
        switch_linked_hosts(keyboard, targets, host, keyboard_transition, channel_pool).await
    }
}

async fn run_transition_with(
    inventory: &mut HostSwitchInventory,
    channel_pool: &ChannelPool,
    receiver_access: &ReceiverAccess,
    link: HostSwitchLink,
    request: HostSwitchRequest,
    executor: &impl HostTransitionExecutor,
    timeouts: TransitionTimeouts,
) {
    let _lease = receiver_access
        .acquire_exclusive(ExclusiveAccessReason::HostTransition)
        .await;
    match executor
        .switch(
            &link.keyboard,
            &link.targets,
            request.host,
            request.keyboard_transition,
            channel_pool,
        )
        .await
    {
        Ok(true) => {
            if !wait_for_departure(inventory, &link.keyboard, timeouts.commanded_departure).await {
                warn!(route = %link.keyboard, "host transition departure was not observed");
            }
        }
        Ok(false) => {}
        Err(HostSwitchError::HostSlotUnverified { .. }) => {
            warn!(
                route = %link.keyboard,
                host = request.host,
                "host destination could not be revalidated; followers remain"
            );
        }
        Err(error) => {
            debug!(%error, route = %link.keyboard, host = request.host, "keyboard host switch failed");
        }
    }
}

fn capture_mode_for(
    announcement_keyboards: &[String],
    keyboard_key: &str,
) -> HostSwitchCaptureMode {
    if announcement_keyboards
        .iter()
        .any(|known_key| known_key == keyboard_key)
    {
        HostSwitchCaptureMode::ChangeHostAnnouncement
    } else {
        HostSwitchCaptureMode::Full
    }
}

fn remember_announcement_keyboard(announcement_keyboards: &mut Vec<String>, keyboard_key: String) {
    if !announcement_keyboards.contains(&keyboard_key) {
        announcement_keyboards.push(keyboard_key);
    }
}

fn keyboard_departed(inventory: &mut HostSwitchInventory, keyboard: &DeviceRoute) -> bool {
    !inventory.borrow_and_update().contains(keyboard)
}

async fn wait_for_departure(
    inventory: &mut HostSwitchInventory,
    keyboard: &DeviceRoute,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        if keyboard_departed(inventory, keyboard) {
            return true;
        }
        tokio::select! {
            result = inventory.changed() => {
                if result.is_err() {
                    return false;
                }
            }
            () = &mut deadline => return false,
        }
    }
}

#[cfg(test)]
mod tests;
