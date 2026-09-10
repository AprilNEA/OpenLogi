//! Keep configured keyboard → pointing-device host-switch links armed.

use std::collections::VecDeque;
use std::thread;
use std::time::Duration;

use openlogi_hid::{
    ChannelPool, ChannelRegistry, DeviceIoGate, DeviceRoute, HostSwitchRequest,
    HostSwitchRestoreOutcome, HostSwitchStopReason, PendingHostSwitchRestore,
    run_host_switch_session, switch_linked_hosts,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use super::shutdown::{ManagerCompletion, WatcherHandle};
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

struct HostSwitchManagerContext {
    links: HostSwitchLinks,
    channel_pool: ChannelPool,
    registry: ChannelRegistry,
    receiver_access: ReceiverAccess,
    receiver_requests: watch::Receiver<ReceiverRequestState>,
    device_io: DeviceIoGate,
    shutdown: oneshot::Receiver<()>,
}

/// Spawn the host switch session manager.
#[must_use]
pub fn spawn(
    links: &HostSwitchLinks,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    device_io: DeviceIoGate,
) -> WatcherHandle {
    let links = links.clone();
    let receiver_requests = receiver_access.subscribe_requests();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (shutdown_done_tx, shutdown_done_rx) = oneshot::channel();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "host switch watcher: could not build tokio runtime");
                let _ = shutdown_done_tx.send(ManagerCompletion::Unexpected);
                return;
            }
        };
        let completion = runtime.block_on(manage(HostSwitchManagerContext {
            links,
            channel_pool,
            registry,
            receiver_access,
            receiver_requests,
            device_io,
            shutdown: shutdown_rx,
        }));
        // A manager return can strand detached task supervisors. Destroy their
        // runtime before reporting that no old firmware writer remains.
        drop(runtime);
        let _ = shutdown_done_tx.send(completion);
    });
    WatcherHandle::new(shutdown_tx, shutdown_done_rx)
}

enum SessionPhase {
    Active(oneshot::Sender<HostSwitchStopReason>),
    Draining,
}

struct RunningSession {
    link: HostSwitchLink,
    generation: u64,
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
    generation: u64,
    requested_host: Option<HostSwitchRequest>,
    restore: RestorePhase,
}

enum HostSwitchSlot {
    Running(RunningSession),
    Recovering(Recovery),
    Restarting {
        link: HostSwitchLink,
        retry_at: Instant,
    },
}

impl HostSwitchSlot {
    fn keyboard(&self) -> &DeviceRoute {
        match self {
            Self::Running(session) => &session.link.keyboard,
            Self::Recovering(recovery) => &recovery.link.keyboard,
            Self::Restarting { link, .. } => &link.keyboard,
        }
    }
}

#[derive(Clone)]
struct TransitionIntent {
    link: HostSwitchLink,
    host: HostSwitchRequest,
}

/// What a finished session's request becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestPlacement {
    /// Run it now, beside any pending restore rather than behind it.
    Scheduled(HostSwitchRequest),
    /// Hand it to the recovery, to run once the restore settles.
    Carried(HostSwitchRequest),
    /// Nothing left to do with it.
    Dropped,
}

/// Decides where a finished session's request goes.
///
/// Two rules, and both exist because an announcement contradicts what the rest
/// of this file assumes. A link is published only while its keyboard is online,
/// and a keyboard announces precisely as it goes offline, so asking whether the
/// link is still listed drops every announcement there will ever be; an
/// announcement stands on the link that was live when the session armed. And a
/// directed request rides along in the recovery, which is the right order while
/// the keyboard is still there to have its controls put back, whereas an
/// announcement's restore cannot finish until the keyboard returns, by which
/// time the pointer has missed the trip it was meant to make.
///
/// A directed request keeps the published check: its keyboard is still here, so
/// a link that has since gone means the user unlinked it.
fn place_request(
    request: Option<HostSwitchRequest>,
    link_is_published: bool,
    has_pending_restore: bool,
    terminal: bool,
) -> RequestPlacement {
    if terminal {
        return RequestPlacement::Dropped;
    }
    match request {
        Some(request @ HostSwitchRequest::Announced { .. }) => RequestPlacement::Scheduled(request),
        Some(request @ HostSwitchRequest::Directed(_)) if link_is_published => {
            if has_pending_restore {
                RequestPlacement::Carried(request)
            } else {
                RequestPlacement::Scheduled(request)
            }
        }
        _ => RequestPlacement::Dropped,
    }
}

impl TransitionIntent {
    /// Whether this intent survives its link leaving the published set.
    ///
    /// A link is published only while its keyboard is online, and an
    /// announcement is what a keyboard sends as it goes offline. Every check
    /// that asks "is this link still listed?" therefore answers no for an
    /// announcement, so an announcement made to pass them all would never run at
    /// all. It stands instead on the link that was live when the session armed.
    ///
    /// A directed request keeps the check: its keyboard is still there, so a
    /// link that has since been unpublished means the user unlinked it, and the
    /// request should die with it.
    fn outlives_its_link(&self) -> bool {
        matches!(self.host, HostSwitchRequest::Announced { .. })
    }
}

/// Queued and in-flight host transitions.
///
/// One runs at a time, because each takes the receiver exclusively, but more
/// than one can be waiting. A single slot used to hold them, and two keyboards
/// announcing close together overwrote one another there: the later completion
/// replaced a queued intent, or replaced a running one whose completion then
/// cleared the slot. Either way one keyboard's followers never moved.
#[derive(Default)]
struct Transitions {
    running: bool,
    queued: VecDeque<TransitionIntent>,
}

impl Transitions {
    /// Queue `intent`, superseding any intent already queued for the same link.
    ///
    /// A keyboard pressed twice before its first transition ran wants the second
    /// destination, not both in order.
    fn schedule(&mut self, intent: TransitionIntent) {
        self.queued.retain(|queued| queued.link != intent.link);
        self.queued.push_back(intent);
    }

    /// Whether anything is queued or in flight.
    fn is_busy(&self) -> bool {
        self.running || !self.queued.is_empty()
    }

    /// Drop what can no longer run: everything on shutdown, and any queued
    /// intent whose link has gone and that does not outlive it.
    fn reconcile(&mut self, published: &[HostSwitchLink], terminal: bool) {
        if terminal {
            self.queued.clear();
            return;
        }
        self.queued
            .retain(|intent| intent.outlives_its_link() || published.contains(&intent.link));
    }

    /// Take the next intent that can run now.
    ///
    /// `None` while one is already running, or while nothing queued can go.
    /// With a restore pending only an announcement can, since a directed
    /// request waits for the keyboard's controls to be put back first.
    ///
    /// Skipping past a blocked directed request rather than stopping at it is
    /// the point: the restore of a keyboard that has left keeps failing and
    /// requeueing, so a directed request sitting at the front would hold every
    /// announcement behind it for as long as that keyboard stays away — which
    /// is exactly as long as the pointers it should have taken are stranded.
    fn begin(&mut self, restores_pending: bool) -> Option<TransitionIntent> {
        if self.running {
            return None;
        }
        let index = self
            .queued
            .iter()
            .position(|intent| !restores_pending || intent.outlives_its_link())?;
        let intent = self.queued.remove(index)?;
        self.running = true;
        Some(intent)
    }

    /// The running transition has finished, whatever its outcome.
    fn finish(&mut self) {
        self.running = false;
    }
}

struct SessionCompletion {
    generation: u64,
    result: Result<SessionResult, tokio::task::JoinError>,
}

struct SessionResult {
    requested_host: Option<HostSwitchRequest>,
    pending_restore: Option<PendingHostSwitchRestore>,
    failed: bool,
}

struct RestoreCompletion {
    generation: u64,
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
    next_generation: u64,
    transitions: Transitions,
    task_failed: bool,
}

impl HostSwitchManagerState {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            next_generation: 0,
            transitions: Transitions::default(),
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
        self.transitions.reconcile(published, terminal);
    }

    fn begin_transition(&mut self, terminal: bool) -> Option<TransitionIntent> {
        if terminal || self.has_running_sessions() {
            return None;
        }
        // A pending restore normally goes first: the keyboard's controls are put
        // back before anything moves hosts. An announcement inverts that. The
        // keyboard has already left, so its restore cannot succeed until it
        // comes back, and by then the pointer that should have travelled with
        // it is long stranded. Letting the transition through is safe because
        // the receiver lock, not this gate, is what keeps the two apart: an
        // exclusive request stops restores from acquiring, and waits out any
        // that already holds.
        self.transitions.begin(self.has_pending_restores())
    }

    fn terminal_completion(&self, terminal: bool) -> Option<ManagerCompletion> {
        (terminal
            && !self.has_running_sessions()
            && !self.has_pending_restores()
            && !self.transitions.is_busy())
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
            let generation = recovery.generation;
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
                    generation,
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
            self.next_generation = self.next_generation.wrapping_add(1);
            self.slots.push(HostSwitchSlot::Running(spawn_session(
                link.clone(),
                self.next_generation,
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
            matches!(slot, HostSwitchSlot::Running(session) if session.generation == completion.generation)
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
        let placement = place_request(
            result.requested_host,
            published.contains(&session.link),
            result.pending_restore.is_some(),
            terminal,
        );
        let request_is_current = placement != RequestPlacement::Dropped;
        if let Some(token) = result.pending_restore {
            if let RequestPlacement::Scheduled(host) = placement {
                self.transitions.schedule(TransitionIntent {
                    link: session.link.clone(),
                    host,
                });
            }
            self.slots.push(HostSwitchSlot::Recovering(Recovery {
                link: session.link,
                generation: session.generation,
                requested_host: match placement {
                    RequestPlacement::Carried(host) => Some(host),
                    _ => None,
                },
                restore: RestorePhase::Ready {
                    token,
                    retry_at: Instant::now() + RETRY_DELAY,
                },
            }));
        } else if let RequestPlacement::Scheduled(host) = placement {
            self.transitions.schedule(TransitionIntent {
                link: session.link,
                host,
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
            matches!(slot, HostSwitchSlot::Recovering(recovery) if recovery.generation == completion.generation)
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
                    self.transitions.schedule(TransitionIntent {
                        link: recovery.link,
                        host,
                    });
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

    loop {
        let requests = *receiver_requests.borrow_and_update();
        let published = std::sync::Arc::clone(&links.borrow_and_update());
        let io_allowed = device_io.allows_io();
        state.reconcile_transition(&published, terminal);
        let wanted = if terminal || requests.any() || state.transitions.is_busy() {
            &[][..]
        } else {
            published.as_slice()
        };
        if io_allowed || terminal {
            state.stop_sessions(wanted, terminal);
        }
        if io_allowed {
            state.reconcile_recoveries(&published, requests, &services, terminal);
            if !terminal && !state.transitions.is_busy() {
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
            state.transitions.finish();
        }
    }
}

fn spawn_session(
    link: HostSwitchLink,
    generation: u64,
    receiver_lease: crate::receiver_access::SessionReceiverLease,
    services: &SessionServices,
) -> RunningSession {
    let (stop, stop_rx) = oneshot::channel();
    let session_link = link.clone();
    let registry = services.registry.clone();
    let device_io = services.device_io.clone();
    let events = services.events.clone();
    tokio::spawn(async move {
        let task = tokio::spawn(async move {
            let _receiver_lease = receiver_lease;
            match run_host_switch_session(
                session_link.keyboard.clone(),
                stop_rx,
                &registry,
                device_io,
            )
            .await
            {
                Ok(outcome) => {
                    let (requested_host, pending_restore) = outcome.into_parts();
                    SessionResult {
                        requested_host,
                        pending_restore,
                        failed: false,
                    }
                }
                Err(failure) => {
                    let (error, pending_restore) = failure.into_parts();
                    debug!(%error, route = %session_link.keyboard, "host switch session ended");
                    SessionResult {
                        requested_host: None,
                        pending_restore,
                        failed: true,
                    }
                }
            }
        });
        let _ = events.send(ManagerEvent::Session(SessionCompletion {
            generation,
            result: task.await,
        }));
    });
    RunningSession {
        link,
        generation,
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
    if !device_io.allows_io() {
        debug!(route = %intent.link.keyboard, "device I/O is suspended — host switch skipped");
        return;
    }
    if !intent.outlives_its_link() && !links.borrow().contains(&intent.link) {
        debug!(route = %intent.link.keyboard, "link is no longer published — host switch skipped");
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
            debug!(%error, route = %intent.link.keyboard, host = ?intent.host, "keyboard host switch failed");
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

async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
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
            generation: 1,
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

    /// The regression this pins down: a single transition slot. Two keyboards
    /// announcing close together overwrote one another there, and one set of
    /// followers simply never moved.
    /// The regression this pins down: taking only the front of the queue. A
    /// directed request waiting on a restore would sit there while the restore
    /// of a keyboard that has left keeps failing and requeueing, and every
    /// announcement behind it, along with the pointers it should have moved,
    /// waited for a keyboard that was not coming back.
    #[test]
    fn an_announcement_is_not_held_up_by_a_directed_request_waiting_on_a_restore() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(1),
            generation: 1,
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));
        state.transitions.schedule(TransitionIntent {
            link: link(1),
            host: HostSwitchRequest::Directed(2),
        });
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Announced { leader_host: 1 },
        });

        let intent = state
            .begin_transition(false)
            .expect("the announcement runs even though the directed request cannot");
        assert_eq!(intent.link, link(2));

        // And the directed request keeps its place rather than being dropped.
        state.transitions.finish();
        assert!(
            state.begin_transition(false).is_none(),
            "it still waits for the restore"
        );
    }

    #[test]
    fn two_keyboards_announcing_together_each_get_their_turn() {
        let mut state = HostSwitchManagerState::new();
        state.transitions.schedule(TransitionIntent {
            link: link(1),
            host: HostSwitchRequest::Announced { leader_host: 0 },
        });
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Announced { leader_host: 1 },
        });

        let first = state
            .begin_transition(false)
            .expect("the first keyboard runs");
        assert_eq!(first.link, link(1));
        assert!(
            state.begin_transition(false).is_none(),
            "one at a time: each transition takes the receiver exclusively"
        );

        state.transitions.finish();

        let second = state
            .begin_transition(false)
            .expect("the second keyboard is still waiting, not forgotten");
        assert_eq!(second.link, link(2));
    }

    /// A keyboard pressed twice before its turn came wants the second
    /// destination, not both of them in order.
    #[test]
    fn a_second_request_from_one_keyboard_supersedes_its_first() {
        let mut state = HostSwitchManagerState::new();
        state.transitions.schedule(TransitionIntent {
            link: link(1),
            host: HostSwitchRequest::Directed(0),
        });
        state.transitions.schedule(TransitionIntent {
            link: link(1),
            host: HostSwitchRequest::Directed(2),
        });

        let intent = state.begin_transition(false).expect("one intent");
        assert_eq!(intent.host, HostSwitchRequest::Directed(2));

        state.transitions.finish();
        assert!(
            state.begin_transition(false).is_none(),
            "the superseded request must not run afterwards"
        );
    }

    #[test]
    fn stale_link_invalidates_transition_intent() {
        let mut state = HostSwitchManagerState::new();
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Directed(1),
        });

        state.reconcile_transition(&[link(3)], false);
        assert!(!state.transitions.is_busy());
        assert!(state.begin_transition(false).is_none());
    }

    #[test]
    fn restoring_firmware_blocks_a_changed_link_for_the_same_keyboard() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(2),
            generation: 1,
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
            generation: 1,
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));

        assert!(state.terminal_completion(true).is_none());
        state.handle_restore_completion(
            RestoreCompletion {
                generation: 0,
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
                generation: 1,
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

    /// The regression this pins down: an announcement used to be handed to the
    /// recovery like any other request, and the recovery only runs its request
    /// once the restore settles. The keyboard has left, so that restore cannot
    /// settle until it returns, and the pointer sat still through the whole
    /// trip.
    #[test]
    fn an_announcement_is_scheduled_beside_a_pending_restore_not_behind_it() {
        let announced = Some(HostSwitchRequest::Announced { leader_host: 1 });

        assert_eq!(
            place_request(announced, false, true, false),
            RequestPlacement::Scheduled(HostSwitchRequest::Announced { leader_host: 1 }),
            "an unpublished link is the normal case for an announcement, not a reason to drop it"
        );
        assert_eq!(
            place_request(announced, true, false, false),
            RequestPlacement::Scheduled(HostSwitchRequest::Announced { leader_host: 1 })
        );
    }

    #[test]
    fn a_directed_request_waits_behind_the_restore_and_needs_its_link() {
        let directed = Some(HostSwitchRequest::Directed(2));

        assert_eq!(
            place_request(directed, true, true, false),
            RequestPlacement::Carried(HostSwitchRequest::Directed(2)),
            "the keyboard is still here, so its controls go back first"
        );
        assert_eq!(
            place_request(directed, true, false, false),
            RequestPlacement::Scheduled(HostSwitchRequest::Directed(2))
        );
        assert_eq!(
            place_request(directed, false, false, false),
            RequestPlacement::Dropped,
            "no link means the user unlinked it while the keyboard was present"
        );
    }

    #[test]
    fn nothing_is_placed_while_shutting_down_or_with_no_request() {
        assert_eq!(
            place_request(
                Some(HostSwitchRequest::Announced { leader_host: 1 }),
                true,
                false,
                true
            ),
            RequestPlacement::Dropped,
            "not even an announcement outruns shutdown"
        );
        assert_eq!(
            place_request(None, true, false, false),
            RequestPlacement::Dropped
        );
    }

    #[test]
    fn an_announced_transition_survives_its_link_being_unpublished() {
        let mut state = HostSwitchManagerState::new();
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Announced { leader_host: 1 },
        });

        // The keyboard going offline is what unpublishes the link, and it is
        // also what produced the announcement, so an empty list here is the
        // normal case rather than a reason to drop the work.
        state.reconcile_transition(&[], false);

        assert!(state.transitions.is_busy());
    }

    #[test]
    fn a_directed_transition_is_dropped_when_its_link_goes_away() {
        let mut state = HostSwitchManagerState::new();
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Directed(2),
        });

        state.reconcile_transition(&[], false);

        assert!(!state.transitions.is_busy(), "the user unlinked it");
    }

    #[test]
    fn an_announced_transition_does_not_wait_for_restoration() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(2),
            generation: 1,
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Announced { leader_host: 1 },
        });

        // The keyboard announced a switch it makes itself and has already gone,
        // so its restore cannot finish until it comes back. Waiting for it would
        // hold the pointer on the machine the user just left.
        assert_eq!(
            state.begin_transition(false).unwrap().host,
            HostSwitchRequest::Announced { leader_host: 1 }
        );
    }

    #[test]
    fn transition_waits_for_restoration_and_keeps_running_until_acknowledged() {
        let mut state = HostSwitchManagerState::new();
        state.slots.push(HostSwitchSlot::Recovering(Recovery {
            link: link(2),
            generation: 1,
            requested_host: None,
            restore: RestorePhase::Restoring,
        }));
        state.transitions.schedule(TransitionIntent {
            link: link(2),
            host: HostSwitchRequest::Directed(2),
        });
        assert!(state.begin_transition(false).is_none());
        assert!(state.transitions.is_busy());

        state.handle_restore_completion(
            RestoreCompletion {
                generation: 1,
                result: Ok(HostSwitchRestoreOutcome::Restored),
            },
            &[link(2)],
            false,
        );
        assert_eq!(
            state.begin_transition(false).unwrap().host,
            HostSwitchRequest::Directed(2)
        );
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
            1,
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
