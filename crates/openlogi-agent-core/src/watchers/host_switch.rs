//! Keep configured keyboard → pointing-device host-switch links armed.

use std::time::Duration;

use openlogi_hid::{
    ChannelPool, ChannelRegistry, DeviceIoGate, DeviceRoute, HostSwitchFollowUp,
    HostSwitchRestoreOutcome, HostSwitchStopReason, PendingHostSwitchRestore,
    run_host_switch_session, switch_linked_hosts,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use super::retry::{RETRY_DELAY, wait_for_deadline};
use super::shutdown::{ManagerCompletion, WatcherHandle};
use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess, ReceiverRequestState};

const DEPARTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// One resolved link. Config keys are converted to live routes by the
/// orchestrator so the transport watcher never needs to understand inventory.
///
/// The keyboard's session moves the targets itself when the keyboard
/// switches by itself. A diverted host key instead becomes a transition that
/// moves the targets and then the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSwitchLink {
    /// Keyboard whose host switch keys initiate the transition.
    pub keyboard: DeviceRoute,
    /// Pointing devices that follow the keyboard.
    pub targets: Vec<DeviceRoute>,
}

/// Read-only, lossless, coalescing view of resolved links.
pub type HostSwitchLinks = watch::Receiver<std::sync::Arc<Vec<HostSwitchLink>>>;

/// A request to move a whole link to another host.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkedSwitchRequest {
    /// The keyboard or one of its targets: the device whose action asked.
    route: DeviceRoute,
    /// The keyboard's zero-based slot. Each target moves to its slot for the
    /// same computer, as when the keyboard's own host key is pressed.
    host: u8,
}

/// Moves a keyboard and the pointing devices linked to it together, the way
/// pressing one of the keyboard's host keys does: the manager restores the
/// keyboard's host keys, moves the targets, and moves the keyboard last.
///
/// Only the latest request is kept, and a request no published link
/// answers for is refused, so the caller can move the one device instead.
#[derive(Clone)]
pub struct LinkedHostSwitch {
    links: HostSwitchLinks,
    requests: std::sync::Arc<watch::Sender<Option<LinkedSwitchRequest>>>,
}

impl LinkedHostSwitch {
    /// A handle over `links`, for the manager [`spawn`] starts.
    #[must_use]
    pub fn new(links: &HostSwitchLinks) -> Self {
        Self {
            links: links.clone(),
            requests: std::sync::Arc::new(watch::channel(None).0),
        }
    }

    /// Ask to move the link `route` belongs to, as its keyboard or one of
    /// its targets, to the keyboard's slot `host`. Returns `false`, and
    /// sends nothing, when no published link has `route`: its keyboard is
    /// offline, or nothing links it.
    #[must_use = "a refused request moves nothing, so the caller must move the device itself"]
    pub fn request(&self, route: &DeviceRoute, host: u8) -> bool {
        if link_for(&self.links.borrow(), route).is_none() {
            return false;
        }
        self.requests.send_replace(Some(LinkedSwitchRequest {
            route: route.clone(),
            host,
        }));
        true
    }

    fn subscribe(&self) -> watch::Receiver<Option<LinkedSwitchRequest>> {
        self.requests.subscribe()
    }
}

impl Default for LinkedHostSwitch {
    /// A handle with no links, which refuses every request.
    fn default() -> Self {
        Self::new(&watch::channel(std::sync::Arc::new(Vec::new())).1)
    }
}

/// The published link that `route` belongs to, as its keyboard or a target.
fn link_for<'a>(
    published: &'a [HostSwitchLink],
    route: &DeviceRoute,
) -> Option<&'a HostSwitchLink> {
    published
        .iter()
        .find(|link| link.keyboard == *route || link.targets.contains(route))
}

struct HostSwitchManagerContext {
    links: HostSwitchLinks,
    switch_requests: watch::Receiver<Option<LinkedSwitchRequest>>,
    channel_pool: ChannelPool,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    receiver_requests: watch::Receiver<ReceiverRequestState>,
    device_io: DeviceIoGate,
    shutdown: oneshot::Receiver<()>,
}

/// Spawn the host switch session manager. It also serves the requests made
/// through `linked`.
#[must_use]
pub fn spawn(
    links: &HostSwitchLinks,
    linked: &LinkedHostSwitch,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    device_io: DeviceIoGate,
) -> WatcherHandle {
    let links = links.clone();
    let switch_requests = linked.subscribe();
    let receiver_requests = receiver_access.subscribe_requests();
    WatcherHandle::spawn("openlogi-host-switch-watcher", move |shutdown| {
        manage(HostSwitchManagerContext {
            links,
            switch_requests,
            channel_pool,
            registry,
            receiver_access,
            receiver_requests,
            device_io,
            shutdown,
        })
    })
}

/// Identity of one spawned host-switch session. A completion settles only the
/// slot carrying its epoch, so a stale task cannot settle its successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SessionEpoch(u64);

impl SessionEpoch {
    fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

enum SessionPhase {
    Active(oneshot::Sender<HostSwitchStopReason>),
    Draining,
}

struct RunningSession {
    link: HostSwitchLink,
    epoch: SessionEpoch,
    phase: SessionPhase,
}

impl RunningSession {
    fn stop(&mut self, reason: HostSwitchStopReason) {
        let SessionPhase::Active(stop) = std::mem::replace(&mut self.phase, SessionPhase::Draining)
        else {
            return;
        };
        let _ = stop.send(reason);
    }
}

enum RestorePhase {
    Ready {
        token: PendingHostSwitchRestore,
        retry_at: Instant,
    },
    Restoring,
}

struct Recovery {
    link: HostSwitchLink,
    epoch: SessionEpoch,
    requested_host: Option<u8>,
    restore: RestorePhase,
}

enum HostSwitchSlot {
    Running(RunningSession),
    Recovering(Recovery),
    Restarting {
        link: HostSwitchLink,
        retry_at: Instant,
    },
    /// The keyboard is switching hosts by itself and its session already
    /// moved the targets. No successor arms until inventory stops publishing
    /// the link or `until` passes: a successor would arm on a keyboard that
    /// is leaving.
    Departing {
        link: HostSwitchLink,
        until: Instant,
    },
}

impl HostSwitchSlot {
    fn keyboard(&self) -> &DeviceRoute {
        match self {
            Self::Running(session) => &session.link.keyboard,
            Self::Recovering(recovery) => &recovery.link.keyboard,
            Self::Restarting { link, .. } | Self::Departing { link, .. } => &link.keyboard,
        }
    }
}

#[derive(Clone)]
struct TransitionIntent {
    link: HostSwitchLink,
    host: u8,
}

enum TransitionPhase {
    Waiting(TransitionIntent),
    Running,
}

struct SessionCompletion {
    epoch: SessionEpoch,
    result: Result<SessionResult, tokio::task::JoinError>,
}

struct SessionResult {
    follow_up: HostSwitchFollowUp,
    pending_restore: Option<PendingHostSwitchRestore>,
    failed: bool,
}

struct RestoreCompletion {
    epoch: SessionEpoch,
    result: Result<HostSwitchRestoreOutcome, tokio::task::JoinError>,
}

enum ManagerEvent {
    Session(SessionCompletion),
    Restore(RestoreCompletion),
    Transition(Result<(), tokio::task::JoinError>),
}

struct SessionServices {
    channel_pool: ChannelPool,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
    events: mpsc::UnboundedSender<ManagerEvent>,
}

struct HostSwitchManagerState {
    slots: Vec<HostSwitchSlot>,
    last_epoch: SessionEpoch,
    transition: Option<TransitionPhase>,
    task_failed: bool,
}

impl HostSwitchManagerState {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            last_epoch: SessionEpoch(0),
            transition: None,
            task_failed: false,
        }
    }

    fn has_pending_restores(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, HostSwitchSlot::Recovering(_)))
    }

    fn has_running_sessions(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, HostSwitchSlot::Running(_)))
    }

    fn owns_keyboard(&self, keyboard: &DeviceRoute) -> bool {
        self.slots.iter().any(|slot| slot.keyboard() == keyboard)
    }

    fn reconcile_transition(&mut self, published: &[HostSwitchLink], terminal: bool) {
        if matches!(
            &self.transition,
            Some(TransitionPhase::Waiting(intent)) if terminal || !published.contains(&intent.link)
        ) {
            self.transition = None;
        }
    }

    /// Queue `request` as a transition of its link, as a diverted host key
    /// queues one: every session then stops and restores its keyboard
    /// before the link moves. A request no published link answers for, or
    /// one that arrives while a transition is already switching, is dropped.
    fn request_switch(&mut self, published: &[HostSwitchLink], request: &LinkedSwitchRequest) {
        let Some(link) = link_for(published, &request.route) else {
            debug!(route = %request.route, "linked host switch for an unlinked device — dropped");
            return;
        };
        if matches!(self.transition, Some(TransitionPhase::Running)) {
            debug!(route = %request.route, "a host transition is already switching — request dropped");
            return;
        }
        self.transition = Some(TransitionPhase::Waiting(TransitionIntent {
            link: link.clone(),
            host: request.host,
        }));
    }

    fn begin_transition(&mut self, terminal: bool) -> Option<TransitionIntent> {
        if terminal || self.has_running_sessions() || self.has_pending_restores() {
            return None;
        }
        let Some(TransitionPhase::Waiting(intent)) = self
            .transition
            .take_if(|phase| matches!(phase, TransitionPhase::Waiting(_)))
        else {
            return None;
        };
        self.transition = Some(TransitionPhase::Running);
        Some(intent)
    }

    fn terminal_completion(&self, terminal: bool) -> Option<ManagerCompletion> {
        (terminal
            && !self.has_running_sessions()
            && !self.has_pending_restores()
            && self.transition.is_none())
        .then_some(if self.task_failed {
            ManagerCompletion::Unexpected
        } else {
            ManagerCompletion::Graceful
        })
    }

    fn deadline(&self, requests: ReceiverRequestState, device_io_allowed: bool) -> Option<Instant> {
        if requests.any() || !device_io_allowed {
            return None;
        }
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                HostSwitchSlot::Recovering(Recovery {
                    restore: RestorePhase::Ready { retry_at, .. },
                    ..
                })
                | HostSwitchSlot::Restarting { retry_at, .. } => Some(*retry_at),
                HostSwitchSlot::Departing { until, .. } => Some(*until),
                HostSwitchSlot::Running(_) | HostSwitchSlot::Recovering(_) => None,
            })
            .min()
    }

    fn stop_sessions(&mut self, wanted: &[HostSwitchLink], terminal: bool) {
        for slot in &mut self.slots {
            let HostSwitchSlot::Running(session) = slot else {
                continue;
            };
            if terminal || !wanted.contains(&session.link) {
                session.stop(HostSwitchStopReason::Graceful);
            }
        }
    }

    fn reconcile_recoveries(
        &mut self,
        published: &[HostSwitchLink],
        requests: ReceiverRequestState,
        services: &SessionServices,
        terminal: bool,
    ) {
        let now = Instant::now();
        self.slots.retain(|slot| match slot {
            HostSwitchSlot::Restarting { link, retry_at } => {
                !terminal && published.contains(link) && (*retry_at > now || requests.any())
            }
            HostSwitchSlot::Departing { link, until } => {
                !terminal && published.contains(link) && *until > now
            }
            HostSwitchSlot::Running(_) | HostSwitchSlot::Recovering(_) => true,
        });
        for slot in &mut self.slots {
            let HostSwitchSlot::Recovering(recovery) = slot else {
                continue;
            };
            if !published.contains(&recovery.link) {
                recovery.requested_host = None;
            }
            let RestorePhase::Ready { retry_at, .. } = &recovery.restore else {
                continue;
            };
            if *retry_at > now || requests.any() {
                continue;
            }
            let Some(lease) = services.receiver_access.try_acquire_for_session() else {
                break;
            };
            let epoch = recovery.epoch;
            let RestorePhase::Ready { token, .. } =
                std::mem::replace(&mut recovery.restore, RestorePhase::Restoring)
            else {
                continue;
            };
            let registry = services.registry.clone();
            let device_io = services.device_io.clone();
            let events = services.events.clone();
            tokio::spawn(async move {
                let task = tokio::spawn(async move {
                    let _lease = lease;
                    if device_io.allows_io() {
                        token.retry(&registry).await
                    } else {
                        HostSwitchRestoreOutcome::RestorePending(token)
                    }
                });
                let _ = events.send(ManagerEvent::Restore(RestoreCompletion {
                    epoch,
                    result: task.await,
                }));
            });
        }
    }

    fn spawn_successors(&mut self, wanted: &[HostSwitchLink], services: &SessionServices) {
        for link in wanted {
            if self.owns_keyboard(&link.keyboard) {
                continue;
            }
            let Some(lease) = services.receiver_access.try_acquire_for_session() else {
                break;
            };
            self.last_epoch = self.last_epoch.next();
            self.slots.push(HostSwitchSlot::Running(spawn_session(
                link.clone(),
                self.last_epoch,
                lease,
                services,
            )));
        }
    }

    fn handle_session_completion(
        &mut self,
        completion: SessionCompletion,
        published: &[HostSwitchLink],
        terminal: bool,
    ) {
        let Some(index) = self.slots.iter().position(|slot| {
            matches!(slot, HostSwitchSlot::Running(session) if session.epoch == completion.epoch)
        }) else {
            return;
        };
        let HostSwitchSlot::Running(session) = self.slots.remove(index) else {
            return;
        };
        let result = match completion.result {
            Ok(result) => result,
            Err(error) => {
                warn!(%error, route = %session.link.keyboard, "host switch session task failed");
                self.task_failed = true;
                return;
            }
        };
        if result.failed {
            debug!(route = %session.link.keyboard, "host switch session ended");
        }
        let request_is_current = !terminal && published.contains(&session.link);
        let requested_host = match result.follow_up {
            HostSwitchFollowUp::Switch(host) => request_is_current.then_some(host),
            HostSwitchFollowUp::Rearm | HostSwitchFollowUp::AwaitDeparture => None,
        };
        let departing = result.follow_up == HostSwitchFollowUp::AwaitDeparture;
        if let Some(token) = result.pending_restore {
            // A keyboard that is leaving cannot answer a restore write. The
            // first retry waits for it to go, unless inventory changes first.
            let first_retry = if departing {
                DEPARTURE_TIMEOUT
            } else {
                RETRY_DELAY
            };
            self.slots.push(HostSwitchSlot::Recovering(Recovery {
                link: session.link,
                epoch: session.epoch,
                requested_host,
                restore: RestorePhase::Ready {
                    token,
                    retry_at: Instant::now() + first_retry,
                },
            }));
        } else if let Some(host) = requested_host {
            self.transition = Some(TransitionPhase::Waiting(TransitionIntent {
                link: session.link,
                host,
            }));
        } else if departing && request_is_current {
            self.slots.push(HostSwitchSlot::Departing {
                link: session.link,
                until: Instant::now() + DEPARTURE_TIMEOUT,
            });
        } else if result.failed && request_is_current {
            self.slots.push(HostSwitchSlot::Restarting {
                link: session.link,
                retry_at: Instant::now() + RETRY_DELAY,
            });
        }
    }

    fn handle_restore_completion(
        &mut self,
        completion: RestoreCompletion,
        published: &[HostSwitchLink],
        terminal: bool,
    ) {
        let Some(index) = self.slots.iter().position(|slot| {
            matches!(slot, HostSwitchSlot::Recovering(recovery) if recovery.epoch == completion.epoch)
        }) else {
            return;
        };
        let HostSwitchSlot::Recovering(mut recovery) = self.slots.remove(index) else {
            return;
        };
        match completion.result {
            Ok(HostSwitchRestoreOutcome::RestorePending(token)) => {
                recovery.restore = RestorePhase::Ready {
                    token,
                    retry_at: Instant::now() + RETRY_DELAY,
                };
                self.slots.push(HostSwitchSlot::Recovering(recovery));
            }
            Ok(HostSwitchRestoreOutcome::Restored) => {
                let request_is_current = !terminal && published.contains(&recovery.link);
                if let Some(host) = recovery.requested_host.filter(|_| request_is_current) {
                    self.transition = Some(TransitionPhase::Waiting(TransitionIntent {
                        link: recovery.link,
                        host,
                    }));
                }
            }
            Err(error) => {
                warn!(%error, route = %recovery.link.keyboard, "host switch restore task failed");
                self.task_failed = true;
            }
        }
    }
}

async fn manage(context: HostSwitchManagerContext) -> ManagerCompletion {
    let HostSwitchManagerContext {
        mut links,
        mut switch_requests,
        channel_pool,
        registry,
        receiver_access,
        mut receiver_requests,
        mut device_io,
        mut shutdown,
    } = context;
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let mut registry_changes = registry.subscribe();
    let services = SessionServices {
        channel_pool,
        registry,
        receiver_access,
        device_io: device_io.clone(),
        events,
    };
    let mut state = HostSwitchManagerState::new();
    let mut terminal = false;
    let mut switch_requests_open = true;

    loop {
        let requests = *receiver_requests.borrow_and_update();
        let published = std::sync::Arc::clone(&links.borrow_and_update());
        let io_allowed = device_io.allows_io();
        state.reconcile_transition(&published, terminal);
        let wanted = if terminal || requests.any() || state.transition.is_some() {
            &[][..]
        } else {
            published.as_slice()
        };
        if io_allowed || terminal {
            state.stop_sessions(wanted, terminal);
        }
        if io_allowed {
            state.reconcile_recoveries(&published, requests, &services, terminal);
            if !terminal && state.transition.is_none() {
                state.spawn_successors(wanted, &services);
            }
        }
        if let Some(completion) = state.terminal_completion(terminal) {
            return completion;
        }
        maybe_spawn_transition(&mut state, &links, &services, terminal);

        let deadline = state.deadline(*receiver_requests.borrow(), device_io.allows_io());
        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            continue;
        }

        tokio::select! {
            biased;

            _ = &mut shutdown, if !terminal => {
                terminal = true;
            }
            Some(event) = event_rx.recv() => {
                let published = links.borrow().clone();
                handle_manager_event(&mut state, event, &published, terminal);
            }
            result = links.changed() => {
                if result.is_err() {
                    return ManagerCompletion::Unexpected;
                }
            }
            result = switch_requests.changed(), if switch_requests_open && !terminal => {
                if result.is_err() {
                    switch_requests_open = false;
                } else if let Some(request) = switch_requests.borrow_and_update().clone() {
                    state.request_switch(&links.borrow(), &request);
                }
            }
            result = receiver_requests.changed() => {
                if result.is_err() {
                    return ManagerCompletion::Unexpected;
                }
            }
            allowed = device_io.changed() => match allowed {
                Some(_) => {}
                None => return ManagerCompletion::Unexpected,
            },
            changed = registry_changes.changed() => {
                if changed.is_err() {
                    return ManagerCompletion::Unexpected;
                }
                expedite_pending_restores(&mut state);
            }
            () = wait_for_deadline(deadline) => {}
        }
    }
}

fn handle_manager_event(
    state: &mut HostSwitchManagerState,
    event: ManagerEvent,
    published: &[HostSwitchLink],
    terminal: bool,
) {
    match event {
        ManagerEvent::Session(completion) => {
            state.handle_session_completion(completion, published, terminal);
        }
        ManagerEvent::Restore(completion) => {
            state.handle_restore_completion(completion, published, terminal);
        }
        ManagerEvent::Transition(result) => {
            if let Err(error) = result {
                warn!(%error, "host transition task failed");
                state.task_failed = true;
            }
            state.transition = None;
        }
    }
}

fn spawn_session(
    link: HostSwitchLink,
    epoch: SessionEpoch,
    receiver_lease: crate::receiver_access::SessionReceiverLease,
    services: &SessionServices,
) -> RunningSession {
    let (stop, stop_rx) = oneshot::channel();
    let session_link = link.clone();
    let registry = services.registry.clone();
    let channel_pool = services.channel_pool.clone();
    let device_io = services.device_io.clone();
    let events = services.events.clone();
    tokio::spawn(async move {
        let task = tokio::spawn(async move {
            let _receiver_lease = receiver_lease;
            match run_host_switch_session(
                session_link.keyboard.clone(),
                &session_link.targets,
                stop_rx,
                &registry,
                &channel_pool,
                device_io,
            )
            .await
            {
                Ok(outcome) => {
                    let (follow_up, pending_restore) = outcome.into_parts();
                    SessionResult {
                        follow_up,
                        pending_restore,
                        failed: false,
                    }
                }
                Err(failure) => {
                    let (error, pending_restore) = failure.into_parts();
                    debug!(%error, route = %session_link.keyboard, "host switch session ended");
                    SessionResult {
                        follow_up: HostSwitchFollowUp::Rearm,
                        pending_restore,
                        failed: true,
                    }
                }
            }
        });
        let _ = events.send(ManagerEvent::Session(SessionCompletion {
            epoch,
            result: task.await,
        }));
    });
    RunningSession {
        link,
        epoch,
        phase: SessionPhase::Active(stop),
    }
}

fn maybe_spawn_transition(
    state: &mut HostSwitchManagerState,
    links: &HostSwitchLinks,
    services: &SessionServices,
    terminal: bool,
) {
    let Some(intent) = state.begin_transition(terminal) else {
        return;
    };
    let links = links.clone();
    let pool = services.channel_pool.clone();
    let receiver_access = services.receiver_access.clone();
    let device_io = services.device_io.clone();
    let events = services.events.clone();
    tokio::spawn(async move {
        let task = tokio::spawn(run_transition(
            links,
            pool,
            receiver_access,
            device_io,
            intent,
        ));
        let _ = events.send(ManagerEvent::Transition(task.await));
    });
}

async fn run_transition(
    mut links: HostSwitchLinks,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
    intent: TransitionIntent,
) {
    let _lease = receiver_access
        .acquire_exclusive(ExclusiveAccessReason::HostTransition)
        .await;
    if !device_io.allows_io() || !links.borrow().contains(&intent.link) {
        return;
    }
    match switch_linked_hosts(
        &intent.link.keyboard,
        &intent.link.targets,
        intent.host,
        &channel_pool,
    )
    .await
    {
        Ok(true) => wait_for_departure(&mut links, &intent.link.keyboard).await,
        Ok(false) => {}
        Err(error) => {
            debug!(%error, route = %intent.link.keyboard, host = intent.host, "keyboard host switch failed");
        }
    }
}

fn expedite_pending_restores(state: &mut HostSwitchManagerState) {
    let now = Instant::now();
    for slot in &mut state.slots {
        if let HostSwitchSlot::Recovering(Recovery {
            restore: RestorePhase::Ready { retry_at, .. },
            ..
        }) = slot
        {
            *retry_at = now;
        }
    }
}

async fn wait_for_departure(links: &mut HostSwitchLinks, keyboard: &DeviceRoute) {
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

    fn link(target: u8) -> HostSwitchLink {
        HostSwitchLink {
            keyboard: route(1),
            targets: vec![route(target)],
        }
    }

    #[test]
    fn target_change_drains_old_session_before_successor_can_arm() {
        let (stop, mut stop_rx) = oneshot::channel();
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Running(RunningSession {
            link: link(2),
            epoch: SessionEpoch(1),
            phase: SessionPhase::Active(stop),
        }));

        state.stop_sessions(&[link(3)], false);

        assert_eq!(
            stop_rx
                .try_recv()
                .expect("old session should begin draining"),
            HostSwitchStopReason::Graceful,
        );
        assert!(state.slots.iter().any(|slot| slot.keyboard() == &route(1)));
    }

    #[test]
    fn a_linked_switch_request_from_either_end_queues_the_link() {
        let published = [link(2), link(3)];
        for asking in [route(1), route(3)] {
            let mut state = HostSwitchManagerState::new();
            state.request_switch(
                &published,
                &LinkedSwitchRequest {
                    route: asking.clone(),
                    host: 1,
                },
            );
            let Some(TransitionPhase::Waiting(intent)) = &state.transition else {
                panic!("{asking} queues a transition");
            };
            assert_eq!(intent.host, 1);
            assert!(
                intent.link.keyboard == route(1),
                "the keyboard leads whichever device asked"
            );
        }
        let mut state = HostSwitchManagerState::new();
        state.request_switch(
            &published,
            &LinkedSwitchRequest {
                route: route(9),
                host: 1,
            },
        );
        assert!(
            state.transition.is_none(),
            "an unlinked device is not queued"
        );
    }

    #[test]
    fn a_linked_switch_request_waits_for_sessions_and_never_preempts_a_running_switch() {
        let (stop, mut stop_rx) = oneshot::channel();
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Running(RunningSession {
            link: link(2),
            epoch: SessionEpoch(1),
            phase: SessionPhase::Active(stop),
        }));
        let request = LinkedSwitchRequest {
            route: route(2),
            host: 2,
        };
        state.request_switch(&[link(2)], &request);
        assert!(
            state.begin_transition(false).is_none(),
            "the keyboard's session restores its host keys first"
        );
        state.stop_sessions(&[], false);
        assert!(stop_rx.try_recv().is_ok(), "the session is asked to stop");

        state.slots.clear();
        assert_eq!(
            state.begin_transition(false).map(|intent| intent.host),
            Some(2)
        );
        state.request_switch(&[link(2)], &LinkedSwitchRequest { host: 0, ..request });
        assert!(
            matches!(state.transition, Some(TransitionPhase::Running)),
            "a running switch is not replaced"
        );
    }

    #[test]
    fn a_linked_switch_handle_refuses_devices_no_published_link_has() {
        let (links_tx, links) = watch::channel(std::sync::Arc::new(vec![link(2)]));
        let linked = LinkedHostSwitch::new(&links);
        let mut requests = linked.subscribe();
        assert!(!linked.request(&route(9), 1));
        assert!(!requests.has_changed().unwrap_or(false));
        assert!(linked.request(&route(2), 1));
        assert_eq!(
            requests.borrow_and_update().clone(),
            Some(LinkedSwitchRequest {
                route: route(2),
                host: 1,
            })
        );
        links_tx.send_replace(std::sync::Arc::new(Vec::new()));
        assert!(!linked.request(&route(2), 1), "the keyboard went offline");
        assert!(!LinkedHostSwitch::default().request(&route(1), 0));
    }

    #[test]
    fn stale_link_invalidates_transition_intent() {
        let mut state = HostSwitchManagerState::new();
        state.transition = Some(TransitionPhase::Waiting(TransitionIntent {
            link: link(2),
            host: 1,
        }));

        state.reconcile_transition(&[link(3)], false);
        assert!(state.transition.is_none());
        assert!(state.begin_transition(false).is_none());
    }

    #[test]
    fn restoring_firmware_blocks_a_changed_link_for_the_same_keyboard() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(2),
            epoch: SessionEpoch(1),
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));

        assert!(
            state.owns_keyboard(&link(3).keyboard),
            "target changes must not permit re-arm over pending keyboard firmware"
        );
    }

    #[test]
    fn terminal_completion_waits_for_restore_acknowledgement() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(2),
            epoch: SessionEpoch(1),
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));

        assert!(state.terminal_completion(true).is_none());
        state.handle_restore_completion(
            RestoreCompletion {
                epoch: SessionEpoch(0),
                result: Ok(HostSwitchRestoreOutcome::Restored),
            },
            &[],
            true,
        );
        assert!(
            state.terminal_completion(true).is_none(),
            "stale completion cannot discard recovery"
        );
        state.handle_restore_completion(
            RestoreCompletion {
                epoch: SessionEpoch(1),
                result: Ok(HostSwitchRestoreOutcome::Restored),
            },
            &[],
            true,
        );
        assert!(matches!(
            state.terminal_completion(true),
            Some(ManagerCompletion::Graceful)
        ));
    }

    #[test]
    fn transition_waits_for_restoration_and_keeps_running_until_acknowledged() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(2),
            epoch: SessionEpoch(1),
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));
        state.transition = Some(TransitionPhase::Waiting(TransitionIntent {
            link: link(2),
            host: 2,
        }));
        assert!(state.begin_transition(false).is_none());
        assert!(matches!(
            state.transition,
            Some(TransitionPhase::Waiting(_))
        ));

        state.handle_restore_completion(
            RestoreCompletion {
                epoch: SessionEpoch(1),
                result: Ok(HostSwitchRestoreOutcome::Restored),
            },
            &[link(2)],
            false,
        );
        assert_eq!(state.begin_transition(false).unwrap().host, 2);
        // Another manager wake while switching must not remove Running.
        assert!(state.begin_transition(false).is_none());
        assert!(state.terminal_completion(true).is_none());
        handle_manager_event(&mut state, ManagerEvent::Transition(Ok(())), &[], true);
        assert!(matches!(
            state.terminal_completion(true),
            Some(ManagerCompletion::Graceful)
        ));
    }

    #[tokio::test]
    async fn completed_session_releases_receiver_lease_before_manager_acknowledgement() {
        let access = ReceiverAccess::default();
        let registry = ChannelRegistry::default();
        let (_signal, gate) = openlogi_hid::device_io_channel();
        let (events, mut received) = mpsc::unbounded_channel();
        let services = SessionServices {
            channel_pool: openlogi_hid::channel_pool(),
            registry,
            receiver_access: access.clone(),
            device_io: gate,
            events,
        };
        let _session = spawn_session(
            link(2),
            SessionEpoch(1),
            access.try_acquire_for_session().unwrap(),
            &services,
        );
        let _exclusive = tokio::time::timeout(
            Duration::from_secs(1),
            access.acquire_exclusive(ExclusiveAccessReason::Pairing),
        )
        .await
        .expect("the failed session must release its lease even before the manager consumes Done");
        let Some(ManagerEvent::Session(completion)) = received.recv().await else {
            panic!("expected session completion");
        };
        assert!(completion.result.unwrap().failed);
    }

    #[test]
    fn suspended_device_io_disables_retry_deadlines() {
        let retry_at = Instant::now() + RETRY_DELAY;
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Restarting {
            link: link(2),
            retry_at,
        });

        assert_eq!(
            state.deadline(ReceiverRequestState::default(), true),
            Some(retry_at)
        );
        assert_eq!(state.deadline(ReceiverRequestState::default(), false), None);
    }

    fn departed(
        state: &mut HostSwitchManagerState,
        pending_restore: Option<PendingHostSwitchRestore>,
    ) {
        let (stop, _stop_rx) = oneshot::channel();
        state.slots.push(HostSwitchSlot::Running(RunningSession {
            link: link(2),
            epoch: SessionEpoch(1),
            phase: SessionPhase::Active(stop),
        }));
        state.handle_session_completion(
            SessionCompletion {
                epoch: SessionEpoch(1),
                result: Ok(SessionResult {
                    follow_up: HostSwitchFollowUp::AwaitDeparture,
                    pending_restore,
                    failed: false,
                }),
            },
            &[link(2)],
            false,
        );
    }

    fn services() -> SessionServices {
        let (_signal, gate) = openlogi_hid::device_io_channel();
        let (events, _received) = mpsc::unbounded_channel();
        SessionServices {
            channel_pool: openlogi_hid::channel_pool(),
            registry: ChannelRegistry::default(),
            receiver_access: ReceiverAccess::default(),
            device_io: gate,
            events,
        }
    }

    #[tokio::test]
    async fn a_departing_keyboard_rearms_only_after_its_link_is_unpublished() {
        let mut state = HostSwitchManagerState::new();
        departed(&mut state, None);

        assert!(
            state.transition.is_none(),
            "the session already moved the targets"
        );
        assert!(
            state.owns_keyboard(&route(1)),
            "no successor may arm on a keyboard that is leaving"
        );
        let services = services();
        state.reconcile_recoveries(
            &[link(2)],
            ReceiverRequestState::default(),
            &services,
            false,
        );
        assert!(
            state.owns_keyboard(&route(1)),
            "the link is still published"
        );

        state.reconcile_recoveries(&[], ReceiverRequestState::default(), &services, false);
        assert!(
            !state.owns_keyboard(&route(1)),
            "a returning keyboard arms a fresh session"
        );
    }

    #[test]
    fn a_departing_keyboard_rearms_after_the_departure_timeout() {
        let mut state = HostSwitchManagerState::new();
        departed(&mut state, None);
        let Some(HostSwitchSlot::Departing { until, .. }) = state.slots.first() else {
            panic!("a departed session leaves a departing slot");
        };
        assert_eq!(
            state.deadline(ReceiverRequestState::default(), true),
            Some(*until),
            "the manager wakes when the wait ends"
        );
    }

    #[tokio::test]
    async fn a_departed_restore_waits_for_the_keyboard_to_go() {
        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let mut state = HostSwitchManagerState::new();
        departed(&mut state, Some(PendingHostSwitchRestore::detached(route)));

        let Some(HostSwitchSlot::Recovering(Recovery {
            requested_host: None,
            restore: RestorePhase::Ready { retry_at, .. },
            ..
        })) = state.slots.first()
        else {
            panic!("a departed session that owes firmware keeps it");
        };
        assert!(
            *retry_at > Instant::now() + RETRY_DELAY,
            "the first retry does not write to a keyboard that is leaving"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn departure_publication_finishes_wait_without_advancing_time() {
        let keyboard = route(1);
        let active = HostSwitchLink {
            keyboard: keyboard.clone(),
            targets: vec![route(2)],
        };
        let (links, mut published) = watch::channel(std::sync::Arc::new(vec![active]));
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
