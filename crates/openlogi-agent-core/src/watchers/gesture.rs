//! Background HID++ control-capture watcher, one session per online device.
//!
//! Runs [`openlogi_hid::run_capture_session`] concurrently for every device in
//! the shared capture-plan list (not just the GUI's selection), restarts a
//! session when its device's plan — route, diverted controls, thumb-wheel
//! arming — changes, and dispatches each captured input against the binding
//! maps of the device it arrived on:
//!
//! - a gesture swipe through the gesture binding map,
//! - a DPI/ModeShift or thumb-wheel-tap press through the button binding map,
//! - thumb-wheel rotation through the
//!   [`ThumbwheelScrollUp`](openlogi_core::binding::ButtonId::ThumbwheelScrollUp) /
//!   [`ThumbwheelScrollDown`](openlogi_core::binding::ButtonId::ThumbwheelScrollDown)
//!   bindings — either re-synthesised as continuous, sensitivity-scaled scroll
//!   or accumulated into a custom action,
//!
//! all via the common [`crate::runtime::ActionDispatcher`].
//!
//! Unlike the CGEventTap hook, this needs no macOS Accessibility permission —
//! the events arrive over HID++, and the bound action is synthesised the same
//! way regardless.

mod dispatch;

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::{
    CaptureChannel, CaptureSessionOutcome, CapturedInput, DeviceIoGate, PendingCaptureRestore,
    run_capture_session_with_registry_spec,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use self::dispatch::InputDispatcher;
use super::capture_session::{CaptureSession, CompletionAction, ReconcileAction};
use crate::capture_plan::{CaptureTarget, DeviceCapturePlan, DispatchPlan, SharedCapturePlans};
use crate::receiver_access::{ReceiverAccess, ReceiverRequestState, SessionReceiverLease};
use crate::runtime::hook::SharedHookMaps;
use crate::runtime::scroll::ScrollInputHandle;
use crate::runtime::{ActionDispatcher, HidppSessionId};

const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Output capabilities shared by every HID++ gesture capture session.
#[derive(Clone)]
pub struct GestureOutputs {
    actions: ActionDispatcher,
    scroll: ScrollInputHandle,
}

impl GestureOutputs {
    /// Build gesture outputs backed by the shared action and scroll runtimes.
    #[must_use]
    pub fn new(actions: ActionDispatcher, scroll: ScrollInputHandle) -> Self {
        Self { actions, scroll }
    }

    fn cancel_session(&self, session: &HidppSessionId) {
        self.actions.cancel_hidpp_session(session);
        self.scroll.cancel_hidpp_session(session);
    }

    fn post_scroll(&self, session: &HidppSessionId, delta: ScrollDelta) {
        if !self.scroll.try_hidpp_scroll(session, delta) {
            // HID++ diversion consumed the physical input already, so direct
            // synthesis is this source's fail-open path.
            openlogi_inject::post_scroll(delta);
        }
    }
}

/// Spawn the capture-manager thread. It owns a current-thread tokio runtime that
/// keeps one capture session pointed at the active device and dispatches each
/// captured input.
pub fn spawn(
    capture_plans: &SharedCapturePlans,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    channel_registry: openlogi_hid::ChannelRegistry,
    device_io: DeviceIoGate,
    outputs: GestureOutputs,
    hook_maps: SharedHookMaps,
) {
    let plans = capture_plans.clone();
    let receiver_requests = receiver_access.subscribe_requests();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "capture watcher: could not build tokio runtime");
                return;
            }
        };
        runtime.block_on(manage(
            plans,
            capture_channel,
            receiver_access,
            receiver_requests,
            channel_registry,
            device_io,
            hook_maps,
            outputs,
        ));
    });
}

type RunningSession = CaptureSession<CaptureTarget, DispatchPlan>;

struct CapturedEvent {
    physical_key: PhysicalDeviceKey,
    session: HidppSessionId,
    input: CapturedInput,
}

struct SessionDone {
    physical_key: PhysicalDeviceKey,
    session: HidppSessionId,
    pending_restore: Option<PendingCaptureRestore>,
}

enum SessionEvent {
    Input(CapturedEvent),
    Done(SessionDone),
}

struct PendingRestore {
    token: PendingCaptureRestore,
    retry_at: Instant,
}

struct GestureManagerState {
    sessions: HashMap<PhysicalDeviceKey, RunningSession>,
    pending_restores: HashMap<PhysicalDeviceKey, PendingRestore>,
    restart_after: HashMap<PhysicalDeviceKey, Instant>,
    input_dispatcher: InputDispatcher,
    lease: std::sync::Weak<SessionReceiverLease>,
}

#[derive(Clone)]
struct SessionChannels {
    events: mpsc::UnboundedSender<SessionEvent>,
    capture: CaptureChannel,
    registry: openlogi_hid::ChannelRegistry,
    device_io: DeviceIoGate,
}

/// Forward one capture session's inputs onto the manager's ordered event
/// channel. The sender closes only after the device listener has been dropped.
fn spawn_input_forwarder(
    physical_key: PhysicalDeviceKey,
    session: HidppSessionId,
    mut inputs: mpsc::UnboundedReceiver<CapturedInput>,
    events: mpsc::UnboundedSender<SessionEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(input) = inputs.recv().await {
            let _ = events.send(SessionEvent::Input(CapturedEvent {
                physical_key: physical_key.clone(),
                session: session.clone(),
                input,
            }));
        }
    })
}

/// Report completion only after every input accepted by the device listener
/// has reached the manager's event channel.
async fn report_done_after_inputs(
    forward_task: tokio::task::JoinHandle<()>,
    events: mpsc::UnboundedSender<SessionEvent>,
    done: SessionDone,
) {
    if let Err(error) = forward_task.await {
        debug!(%error, "capture input forwarder ended unexpectedly");
    }
    let _ = events.send(SessionEvent::Done(done));
}

/// Return the plan that owns an input from the currently tracked session. An
/// active session follows compatible plan updates; a deliberately stopped
/// session keeps its frozen plan and remains admissible until its task reports
/// that native firmware reporting has been restored.
fn dispatch_context_for<'a>(
    input_session: &HidppSessionId,
    live: Option<&'a RunningSession>,
) -> Option<(&'a HidppSessionId, &'a DispatchPlan)> {
    live.filter(|session| session.owns(input_session))
        .map(|session| (session.id(), session.dispatch()))
}

/// Snapshot the sessions that should be armed. An exclusive request
/// temporarily makes the wanted set empty so normal teardown restores every
/// control.
#[cfg(test)]
fn wanted_sessions(
    requests: ReceiverRequestState,
    capture_plans: &watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
) -> Arc<Vec<DeviceCapturePlan>> {
    if requests.any() {
        return Arc::new(Vec::new());
    }
    Arc::clone(&capture_plans.borrow())
}

fn reconcile_session(
    session: &mut RunningSession,
    wanted: Option<(&CaptureTarget, &DispatchPlan)>,
    dispatcher: &mut InputDispatcher,
) {
    if session.reconcile(wanted) == ReconcileAction::DispatchChanged {
        dispatcher.cancel_session(session.id());
        let config_key = session.dispatch().config_key.clone();
        session.rekey(&config_key);
    }
}

/// Reconcile one tracked slot directly against the latest publication. Input
/// calls this before dispatch so an event cannot slip between publishing a hot
/// action update and processing its notification.
fn reconcile_published_session(
    key: &PhysicalDeviceKey,
    session: &mut RunningSession,
    receiver_requests: &watch::Receiver<ReceiverRequestState>,
    capture_plans: &watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
    dispatcher: &mut InputDispatcher,
) {
    if receiver_requests.borrow().any() {
        reconcile_session(session, None, dispatcher);
    } else {
        let plans = capture_plans.borrow();
        let wanted = plans
            .iter()
            .find(|plan| plan.target.physical_key == *key)
            .map(|plan| (&plan.target, &plan.dispatch));
        reconcile_session(session, wanted, dispatcher);
    }
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn wait_for_registry_change(
    changes: &mut watch::Receiver<()>,
    has_pending_restore: bool,
) -> bool {
    if !has_pending_restore {
        return std::future::pending().await;
    }
    changes.changed().await.is_ok()
}

fn acquire_session_lease(
    receiver_access: &ReceiverAccess,
    lease: &mut std::sync::Weak<SessionReceiverLease>,
) -> Option<Arc<SessionReceiverLease>> {
    if let Some(existing) = lease.upgrade() {
        return Some(existing);
    }
    let fresh = Arc::new(receiver_access.try_acquire_for_session()?);
    *lease = Arc::downgrade(&fresh);
    Some(fresh)
}

async fn retry_pending_restores(
    pending_restores: &mut HashMap<PhysicalDeviceKey, PendingRestore>,
    registry: &openlogi_hid::ChannelRegistry,
    now: Instant,
) {
    let keys: Vec<_> = pending_restores
        .iter()
        .filter(|(_, pending)| pending.retry_at <= now)
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        let Some(pending) = pending_restores.remove(&key) else {
            continue;
        };
        if let CaptureSessionOutcome::RestorePending(token) = pending.token.retry(registry).await {
            pending_restores.insert(
                key,
                PendingRestore {
                    token,
                    retry_at: Instant::now() + RETRY_DELAY,
                },
            );
        }
    }
}

fn next_deadline(
    requests: ReceiverRequestState,
    device_io_allowed: bool,
    pending_restores: &HashMap<PhysicalDeviceKey, PendingRestore>,
    restart_after: &HashMap<PhysicalDeviceKey, Instant>,
) -> Option<Instant> {
    if requests.any() || !device_io_allowed {
        return None;
    }
    pending_restores
        .values()
        .map(|pending| pending.retry_at)
        .chain(restart_after.values().copied())
        .min()
}

fn restart_deadline(unexpected: bool, now: Instant) -> Option<Instant> {
    unexpected.then_some(now + RETRY_DELAY)
}

impl GestureManagerState {
    fn new(hook_maps: SharedHookMaps, outputs: GestureOutputs) -> Self {
        Self {
            sessions: HashMap::new(),
            pending_restores: HashMap::new(),
            restart_after: HashMap::new(),
            input_dispatcher: InputDispatcher::new(hook_maps, outputs),
            lease: std::sync::Weak::new(),
        }
    }

    fn deadline(&self, requests: ReceiverRequestState, device_io_allowed: bool) -> Option<Instant> {
        next_deadline(
            requests,
            device_io_allowed,
            &self.pending_restores,
            &self.restart_after,
        )
    }

    fn expedite_pending_restores(&mut self) {
        let now = Instant::now();
        for pending in self.pending_restores.values_mut() {
            pending.retry_at = now;
        }
    }

    async fn reconcile(
        &mut self,
        requests: ReceiverRequestState,
        device_io_allowed: bool,
        published: &Arc<Vec<DeviceCapturePlan>>,
        receiver_access: &ReceiverAccess,
        channels: &SessionChannels,
    ) {
        // Keep existing passive listeners and their firmware ownership intact
        // while the display/session is asleep. Retiring them here would issue
        // restoration writes during DarkWake; retries and successors wait for
        // the user-visible resume instead.
        if !device_io_allowed {
            return;
        }
        let now = Instant::now();
        let wanted = if requests.any() {
            &[][..]
        } else {
            published.as_slice()
        };
        for (key, session) in &mut self.sessions {
            let wanted = wanted
                .iter()
                .find(|plan| plan.target.physical_key == *key)
                .map(|plan| (&plan.target, &plan.dispatch));
            reconcile_session(session, wanted, &mut self.input_dispatcher);
        }
        self.restart_after.retain(|key, _| {
            published
                .iter()
                .any(|plan| plan.target.physical_key == *key)
        });

        // Firmware ownership outlives the desired plan. Keep the strong lease
        // through successor spawning so restore→rearm is uninterrupted.
        let due_restore = self
            .pending_restores
            .values()
            .any(|pending| pending.retry_at <= now);
        let restore_lease = if due_restore {
            acquire_session_lease(receiver_access, &mut self.lease)
        } else {
            None
        };
        if restore_lease.is_some() {
            retry_pending_restores(&mut self.pending_restores, &channels.registry, now).await;
        }

        for plan in wanted {
            let key = &plan.target.physical_key;
            if self.sessions.contains_key(key) || self.pending_restores.contains_key(key) {
                continue;
            }
            if self
                .restart_after
                .get(key)
                .is_some_and(|deadline| *deadline > now)
            {
                continue;
            }
            self.restart_after.remove(key);
            let Some(session_lease) = acquire_session_lease(receiver_access, &mut self.lease)
            else {
                self.restart_after.insert(key.clone(), now + RETRY_DELAY);
                continue;
            };
            let id = HidppSessionId::new(&plan.dispatch.config_key);
            let session = spawn_session(id, plan.clone(), session_lease, channels);
            self.sessions.insert(key.clone(), session);
        }
    }

    fn handle_session_event(
        &mut self,
        event: SessionEvent,
        device_io_allowed: bool,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        capture_plans: &watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
    ) -> bool {
        match event {
            SessionEvent::Input(event) => {
                let key = &event.physical_key;
                if device_io_allowed && let Some(session) = self.sessions.get_mut(key) {
                    reconcile_published_session(
                        key,
                        session,
                        receiver_requests,
                        capture_plans,
                        &mut self.input_dispatcher,
                    );
                }
                let live = self.sessions.get(key);
                let dispatch_context = dispatch_context_for(&event.session, live);
                if let Some((session, plan)) = dispatch_context {
                    self.input_dispatcher.dispatch(session, plan, event.input);
                } else {
                    self.input_dispatcher.cancel_session(&event.session);
                    debug!(
                        key = key.as_str(),
                        epoch = event.session.epoch(),
                        "input from a stale capture session — ignored"
                    );
                }
                false
            }
            SessionEvent::Done(done) => {
                let key = &done.physical_key;
                // Completion is queued behind every input the listener
                // accepted during restoration, so cancellation cannot
                // overtake the last diverted edge.
                let Some((CompletionAction::Remove { unexpected }, dispatch_session)) = self
                    .sessions
                    .get(key)
                    .map(|session| (session.completion(&done.session), session.id().clone()))
                else {
                    return false;
                };
                if let Some(pending) = done.pending_restore {
                    self.pending_restores.insert(
                        key.clone(),
                        PendingRestore {
                            token: pending,
                            retry_at: Instant::now() + RETRY_DELAY,
                        },
                    );
                }
                self.input_dispatcher.cancel_session(&dispatch_session);
                if device_io_allowed
                    && let Some(deadline) = restart_deadline(unexpected, Instant::now())
                {
                    self.restart_after.insert(key.clone(), deadline);
                    warn!(
                        key = key.as_str(),
                        "capture session ended unexpectedly, delaying re-arm"
                    );
                }
                self.sessions.remove(key);
                true
            }
        }
    }
}

/// Keep one capture session alive per online device, restarting a session when
/// its device's plan changes, and dispatch incoming inputs against the plan of
/// the device they arrived on. Runs for the lifetime of the process.
async fn manage(
    mut capture_plans: watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    mut receiver_requests: watch::Receiver<ReceiverRequestState>,
    channel_registry: openlogi_hid::ChannelRegistry,
    mut device_io: DeviceIoGate,
    hook_maps: SharedHookMaps,
    outputs: GestureOutputs,
) {
    let (events, mut event_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let mut registry_changes = channel_registry.subscribe();
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed. Each session reports its completion here, tagged with its device
    // key and the epoch it started under: a dead *current* session re-arms on the
    // retry deadline, a deliberately stopped one immediately frees its key for the
    // replacement once its teardown has drained, and stale completions are
    // ignored by the shared capture-session lifecycle.
    let channels = SessionChannels {
        events,
        capture: capture_channel,
        registry: channel_registry,
        device_io: device_io.clone(),
    };
    let mut state = GestureManagerState::new(hook_maps, outputs);
    let mut reconcile = true;

    loop {
        if reconcile {
            reconcile = false;
            let device_io_allowed = device_io.allows_io();
            if device_io_allowed {
                let requests = *receiver_requests.borrow_and_update();
                let published = Arc::clone(&capture_plans.borrow_and_update());
                state
                    .reconcile(
                        requests,
                        device_io_allowed,
                        &published,
                        &receiver_access,
                        &channels,
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
            Some(event) = event_rx.recv() => {
                reconcile |= state.handle_session_event(
                    event,
                    device_io.allows_io(),
                    &receiver_requests,
                    &capture_plans,
                );
            }
            result = capture_plans.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            result = receiver_requests.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            allowed = device_io.changed() => match allowed {
                Some(true) => reconcile = true,
                Some(false) => {}
                None => return,
            },
            open = wait_for_registry_change(
                &mut registry_changes,
                !state.pending_restores.is_empty(),
            ) => {
                if !open {
                    return;
                }
                if device_io.allows_io() {
                    state.expedite_pending_restores();
                    reconcile = true;
                }
            }
            () = wait_for_deadline(deadline) => {
                reconcile = true;
            }
        }
    }
}

/// Start one device's capture session plus its input-forwarding task, and
/// return the manager's tracking entry for it.
fn spawn_session(
    id: HidppSessionId,
    plan: DeviceCapturePlan,
    lease: Arc<SessionReceiverLease>,
    channels: &SessionChannels,
) -> RunningSession {
    let DeviceCapturePlan {
        target, dispatch, ..
    } = plan;
    let physical_key = target.physical_key.clone();
    let (stop_tx, stop_rx) = oneshot::channel();
    // Tag this session's inputs with its device key so dispatch resolves them
    // against the right plan.
    let (session_tx, session_rx) = mpsc::unbounded_channel::<CapturedInput>();
    let forward_task = spawn_input_forwarder(
        physical_key.clone(),
        id.clone(),
        session_rx,
        channels.events.clone(),
    );
    let events = channels.events.clone();
    let done_id = id.clone();
    let done_key = physical_key;
    let session_route = target.route.clone();
    let session_spec = target.spec.clone();
    let slot = Arc::clone(&channels.capture);
    let registry = channels.registry.clone();
    let device_io = channels.device_io.clone();
    tokio::spawn(async move {
        let _lease = lease;
        let pending_restore = match run_capture_session_with_registry_spec(
            session_route,
            session_spec,
            session_tx,
            stop_rx,
            slot,
            &registry,
            device_io,
        )
        .await
        {
            Ok(CaptureSessionOutcome::Restored) => None,
            Ok(CaptureSessionOutcome::RestorePending(pending)) => Some(pending),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                debug!(%error, "capture session ended");
                pending
            }
        };
        // Use the same channel as input so completion follows every diverted
        // report accepted before the listener was dropped.
        report_done_after_inputs(
            forward_task,
            events,
            SessionDone {
                physical_key: done_key,
                session: done_id,
                pending_restore,
            },
        )
        .await;
    });
    CaptureSession::active(id, target, dispatch, stop_tx)
}

#[cfg(test)]
mod tests;
