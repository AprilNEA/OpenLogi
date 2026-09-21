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

use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::{
    CaptureHost, CaptureSessionOutcome, CapturedInput, PendingCaptureRestore, run_capture_session,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{debug, warn};

use self::dispatch::InputDispatcher;
use super::capture_manager::{self, CaptureManager, ManagerInputs, PendingRestore};
use super::capture_session::{CaptureRecovery, CaptureSession, CaptureSlot, ReconcileAction};
use super::retry::RETRY_DELAY;
use super::shutdown::{ManagerCompletion, WatcherHandle};
use crate::capture_plan::{CaptureTarget, DeviceCapturePlan, DispatchPlan, SharedCapturePlans};
use crate::hardware::DeviceAccess;
use crate::receiver_access::{ReceiverAccess, ReceiverRequestState, SessionReceiverLease};
use crate::runtime::hook::SharedHookMaps;
use crate::runtime::scroll::ScrollInputHandle;
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Output capabilities shared by every HID++ gesture capture session.
#[derive(Clone)]
pub struct GestureOutputs {
    actions: ActionDispatcher,
    scroll: ScrollInputHandle,
    hook_maps: SharedHookMaps,
}

impl GestureOutputs {
    /// Build gesture outputs backed by the shared action and scroll runtimes.
    #[must_use]
    pub fn new(
        actions: ActionDispatcher,
        scroll: ScrollInputHandle,
        hook_maps: SharedHookMaps,
    ) -> Self {
        Self {
            actions,
            scroll,
            hook_maps,
        }
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
#[must_use]
pub fn spawn(
    capture_plans: &SharedCapturePlans,
    access: DeviceAccess,
    outputs: GestureOutputs,
) -> WatcherHandle {
    let plans = capture_plans.clone();
    let receiver_requests = access.receiver_access.subscribe_requests();
    WatcherHandle::spawn("openlogi-gesture-watcher", move |shutdown| {
        manage(GestureManagerContext {
            capture_plans: plans,
            access,
            receiver_requests,
            outputs,
            shutdown,
        })
    })
}

type RunningSession = CaptureSession<CaptureTarget, DispatchPlan>;
type GestureSlot = CaptureSlot<CaptureTarget, DispatchPlan, PendingRestore>;

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

struct GestureManagerState {
    slots: HashMap<PhysicalDeviceKey, GestureSlot>,
    input_dispatcher: InputDispatcher,
    lease: std::sync::Weak<SessionReceiverLease>,
}

#[derive(Clone)]
struct SessionChannels {
    events: mpsc::UnboundedSender<SessionEvent>,
    access: DeviceAccess,
}

struct GestureManagerContext {
    capture_plans: watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
    access: DeviceAccess,
    receiver_requests: watch::Receiver<ReceiverRequestState>,
    outputs: GestureOutputs,
    shutdown: oneshot::Receiver<()>,
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
    slots: &mut HashMap<PhysicalDeviceKey, GestureSlot>,
    registry: &openlogi_hid::ChannelRegistry,
    now: Instant,
) {
    let keys: Vec<_> = slots
        .iter()
        .filter(|(_, slot)| {
            slot.recovery()
                .and_then(|recovery| recovery.pending_restore.as_ref())
                .is_some_and(|pending| pending.retry_at <= now)
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        let Some(GestureSlot::Recovering(mut recovery)) = slots.remove(&key) else {
            continue;
        };
        let Some(pending) = recovery.pending_restore.take() else {
            slots.insert(key, GestureSlot::Recovering(recovery));
            continue;
        };
        if let CaptureSessionOutcome::RestorePending(token) = pending.token.retry(registry).await {
            recovery.pending_restore = Some(PendingRestore {
                token,
                retry_at: Instant::now() + RETRY_DELAY,
            });
        }
        if !recovery.is_empty() {
            slots.insert(key, GestureSlot::Recovering(recovery));
        }
    }
}

fn next_deadline(
    requests: ReceiverRequestState,
    device_io_allowed: bool,
    slots: &HashMap<PhysicalDeviceKey, GestureSlot>,
) -> Option<Instant> {
    if requests.any() || !device_io_allowed {
        return None;
    }
    slots
        .values()
        .filter_map(GestureSlot::recovery)
        .filter_map(|recovery| recovery.next_deadline(|pending| pending.retry_at))
        .min()
}

impl GestureManagerState {
    fn new(outputs: GestureOutputs) -> Self {
        Self {
            slots: HashMap::new(),
            input_dispatcher: InputDispatcher::new(outputs),
            lease: std::sync::Weak::new(),
        }
    }

    fn deadline(&self, requests: ReceiverRequestState, device_io_allowed: bool) -> Option<Instant> {
        next_deadline(requests, device_io_allowed, &self.slots)
    }

    fn expedite_pending_restores(&mut self) {
        let now = Instant::now();
        for slot in self.slots.values_mut() {
            if let Some(pending) = slot
                .recovery_mut()
                .and_then(|recovery| recovery.pending_restore.as_mut())
            {
                pending.retry_at = now;
            }
        }
    }

    fn has_pending_restores(&self) -> bool {
        self.slots.values().any(|slot| {
            slot.recovery()
                .is_some_and(|recovery| recovery.pending_restore.is_some())
        })
    }

    async fn reconcile(
        &mut self,
        requests: ReceiverRequestState,
        device_io_allowed: bool,
        published: &Arc<Vec<DeviceCapturePlan>>,
        channels: &SessionChannels,
    ) {
        let receiver_access = &channels.access.receiver_access;
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
        for (key, slot) in &mut self.slots {
            let Some(session) = slot.session_mut() else {
                continue;
            };
            let wanted = wanted
                .iter()
                .find(|plan| plan.target.physical_key == *key)
                .map(|plan| (&plan.target, &plan.dispatch));
            reconcile_session(session, wanted, &mut self.input_dispatcher);
        }
        self.slots.retain(|key, slot| {
            let Some(recovery) = slot.recovery_mut() else {
                return true;
            };
            if !published
                .iter()
                .any(|plan| plan.target.physical_key == *key)
            {
                recovery.restart_at = None;
            }
            !recovery.is_empty()
        });

        // Firmware ownership outlives the desired plan. Keep the strong lease
        // through successor spawning so restore→rearm is uninterrupted.
        let due_restore = self.slots.values().any(|slot| {
            slot.recovery()
                .and_then(|recovery| recovery.pending_restore.as_ref())
                .is_some_and(|pending| pending.retry_at <= now)
        });
        let restore_lease = if due_restore {
            acquire_session_lease(receiver_access, &mut self.lease)
        } else {
            None
        };
        if restore_lease.is_some() {
            retry_pending_restores(&mut self.slots, &channels.access.registry, now).await;
        }

        for plan in wanted {
            let key = &plan.target.physical_key;
            if self.slots.get(key).is_some_and(|slot| match slot {
                GestureSlot::Running(_) => true,
                GestureSlot::Recovering(recovery) => recovery.blocks_restart(now),
            }) {
                continue;
            }
            let Some(session_lease) = acquire_session_lease(receiver_access, &mut self.lease)
            else {
                self.slots.insert(
                    key.clone(),
                    GestureSlot::recovering(None, Some(now + RETRY_DELAY)),
                );
                continue;
            };
            let id = HidppSessionId::new(&plan.dispatch.config_key);
            let session = spawn_session(id, plan.clone(), session_lease, channels);
            self.slots
                .insert(key.clone(), GestureSlot::running(session));
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
                if device_io_allowed
                    && let Some(session) =
                        self.slots.get_mut(key).and_then(GestureSlot::session_mut)
                {
                    reconcile_published_session(
                        key,
                        session,
                        receiver_requests,
                        capture_plans,
                        &mut self.input_dispatcher,
                    );
                }
                let live = self.slots.get(key).and_then(GestureSlot::session);
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
                let now = Instant::now();
                let pending_restore = done.pending_restore.map(|token| PendingRestore {
                    token,
                    retry_at: now + RETRY_DELAY,
                });
                let restart_at = device_io_allowed.then_some(now + RETRY_DELAY);
                let Some(slot) = self.slots.get_mut(key) else {
                    return false;
                };
                let Some((dispatch_session, unexpected)) =
                    slot.complete(&done.session, pending_restore, restart_at)
                else {
                    return false;
                };
                let recovery_finished = slot.recovery().is_some_and(CaptureRecovery::is_empty);
                self.input_dispatcher.cancel_session(&dispatch_session);
                if unexpected && device_io_allowed {
                    warn!(
                        key = key.as_str(),
                        "capture session ended unexpectedly, delaying re-arm"
                    );
                }
                if recovery_finished {
                    self.slots.remove(key);
                }
                true
            }
        }
    }
}

/// Stop every tracked capture epoch and wait until each session task has
/// reported ordered completion. Retain and retry pending restore tokens until
/// the firmware ownership has been released.
async fn drain_for_shutdown(
    state: &mut GestureManagerState,
    event_rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
    receiver_requests: &watch::Receiver<ReceiverRequestState>,
    capture_plans: &watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
    channels: &SessionChannels,
) {
    for session in state
        .slots
        .values_mut()
        .filter_map(GestureSlot::session_mut)
    {
        reconcile_session(session, None, &mut state.input_dispatcher);
    }
    while state.slots.values().any(|slot| slot.session().is_some()) {
        let Some(event) = event_rx.recv().await else {
            break;
        };
        state.handle_session_event(
            event,
            channels.access.device_io.allows_io(),
            receiver_requests,
            capture_plans,
        );
    }

    state.expedite_pending_restores();
    if state.has_pending_restores() {
        warn!("capture watcher is waiting for pending firmware restoration before stopping");
    }
    let mut device_io = channels.access.device_io.clone();
    while state.has_pending_restores() {
        if !device_io.allows_io() && !device_io.wait_until_allowed().await {
            // A closed suspended gate cannot prove firmware is native. The
            // process lifecycle bounds terminal exits; confirmed replacement
            // deliberately remains here rather than losing these tokens.
            std::future::pending::<()>().await;
        }
        if let Some(_lease) =
            acquire_session_lease(&channels.access.receiver_access, &mut state.lease)
        {
            retry_pending_restores(&mut state.slots, &channels.access.registry, Instant::now())
                .await;
        }
        if state.has_pending_restores() {
            tokio::time::sleep(RETRY_DELAY).await;
            state.expedite_pending_restores();
        }
    }
}

/// The gesture manager as the shared loop drives it: many slots, one weak
/// receiver lease shared by every session.
struct GestureManager {
    state: GestureManagerState,
    channels: SessionChannels,
}

impl CaptureManager for GestureManager {
    type Published = Arc<Vec<DeviceCapturePlan>>;
    type Event = SessionEvent;

    async fn reconcile(&mut self, requests: ReceiverRequestState, published: &Self::Published) {
        self.state
            .reconcile(requests, true, published, &self.channels)
            .await;
    }

    fn handle_session_event(
        &mut self,
        event: SessionEvent,
        device_io_allowed: bool,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        published: &watch::Receiver<Self::Published>,
    ) -> bool {
        self.state
            .handle_session_event(event, device_io_allowed, receiver_requests, published)
    }

    fn deadline(&self, requests: ReceiverRequestState, device_io_allowed: bool) -> Option<Instant> {
        self.state.deadline(requests, device_io_allowed)
    }

    fn has_pending_restores(&self) -> bool {
        self.state.has_pending_restores()
    }

    fn expedite_pending_restores(&mut self) {
        self.state.expedite_pending_restores();
    }

    async fn drain_for_shutdown(
        &mut self,
        events: &mut mpsc::UnboundedReceiver<SessionEvent>,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        published: &watch::Receiver<Self::Published>,
    ) {
        drain_for_shutdown(
            &mut self.state,
            events,
            receiver_requests,
            published,
            &self.channels,
        )
        .await;
    }
}

/// Keep one capture session alive per online device, restarting a session when
/// its device's plan changes, and dispatch incoming inputs against the plan of
/// the device they arrived on. Runs for the lifetime of the process.
async fn manage(context: GestureManagerContext) -> ManagerCompletion {
    let GestureManagerContext {
        capture_plans,
        access,
        receiver_requests,
        outputs,
        shutdown,
    } = context;
    let (events, event_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let registry_changes = access.registry.subscribe();
    let device_io = access.device_io.clone();
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed. Each session reports its completion here, tagged with its device
    // key and the epoch it started under: a dead *current* session re-arms on the
    // retry deadline, a deliberately stopped one immediately frees its key for the
    // replacement once its teardown has drained, and stale completions are
    // ignored by the shared capture-session lifecycle.
    let channels = SessionChannels { events, access };
    capture_manager::run(
        GestureManager {
            state: GestureManagerState::new(outputs),
            channels,
        },
        ManagerInputs {
            published: capture_plans,
            receiver_requests,
            registry_changes,
            device_io,
            events: event_rx,
            shutdown,
        },
    )
    .await
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
    let slot = Arc::clone(&channels.access.channel);
    let registry = channels.access.registry.clone();
    let device_io = channels.access.device_io.clone();
    tokio::spawn(async move {
        let _lease = lease;
        let pending_restore = match run_capture_session(
            session_route,
            session_spec,
            CaptureHost {
                sink: session_tx,
                shutdown: stop_rx,
                channel_slot: slot,
                registry: &registry,
                device_io,
            },
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
