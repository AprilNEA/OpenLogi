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
    CaptureChannel, CaptureSessionOutcome, CapturedInput, PendingCaptureRestore,
    run_capture_session_with_registry_spec,
};
use tokio::sync::{Notify, mpsc, oneshot};
use tracing::{debug, warn};

use self::dispatch::InputDispatcher;
use super::capture_session::{CaptureSession, CompletionAction, ReconcileAction};
use crate::capture_plan::{CaptureTarget, DeviceCapturePlan, DispatchPlan, SharedCapturePlans};
use crate::receiver_access::{ReceiverAccess, SessionReceiverLease};
use crate::runtime::scroll::ScrollInputHandle;
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Fallback interval for reconciling a missed plan notification and pacing the
/// respawn of a session that ended on its own (see `manage`).
const TARGET_POLL: Duration = Duration::from_secs(1);

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
    capture_plans: SharedCapturePlans,
    capture_plan_changed: Arc<Notify>,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    channel_registry: openlogi_hid::ChannelRegistry,
    outputs: GestureOutputs,
) {
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
            capture_plans,
            capture_plan_changed,
            capture_channel,
            receiver_access,
            channel_registry,
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

#[derive(Clone)]
struct SessionChannels {
    events: mpsc::UnboundedSender<SessionEvent>,
    capture: CaptureChannel,
    registry: openlogi_hid::ChannelRegistry,
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

/// Snapshot the sessions that should be armed on this tick. Pairing owns the
/// receiver exclusively, so its request temporarily makes the wanted set
/// empty and lets the normal teardown path restore every control.
fn wanted_sessions(
    receiver_access: &ReceiverAccess,
    capture_plans: &SharedCapturePlans,
) -> HashMap<PhysicalDeviceKey, DeviceCapturePlan> {
    if receiver_access.exclusive_requested() {
        return HashMap::new();
    }
    capture_plans
        .read()
        .map(|plans| {
            plans
                .iter()
                .map(|plan| (plan.target.physical_key.clone(), plan.clone()))
                .collect()
        })
        .unwrap_or_default()
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
/// calls this before dispatch so an event cannot slip through the interval
/// between publishing a hot action update and processing its notification.
fn reconcile_published_session(
    key: &PhysicalDeviceKey,
    session: &mut RunningSession,
    receiver_access: &ReceiverAccess,
    capture_plans: &SharedCapturePlans,
    dispatcher: &mut InputDispatcher,
) {
    if receiver_access.exclusive_requested() {
        reconcile_session(session, None, dispatcher);
    } else {
        match capture_plans.read() {
            Ok(plans) => {
                let wanted = plans
                    .iter()
                    .find(|plan| plan.target.physical_key == *key)
                    .map(|plan| (&plan.target, &plan.dispatch));
                reconcile_session(session, wanted, dispatcher);
            }
            Err(_) => reconcile_session(session, None, dispatcher),
        }
    }
}

async fn wait_for_reconcile(ticker: &mut tokio::time::Interval, changed: &Notify) {
    tokio::select! {
        _ = ticker.tick() => {}
        () = changed.notified() => {}
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
    pending_restores: &mut HashMap<PhysicalDeviceKey, PendingCaptureRestore>,
    registry: &openlogi_hid::ChannelRegistry,
) {
    let keys: Vec<_> = pending_restores.keys().cloned().collect();
    for key in keys {
        let Some(pending) = pending_restores.remove(&key) else {
            continue;
        };
        if let CaptureSessionOutcome::RestorePending(pending) = pending.retry(registry).await {
            pending_restores.insert(key, pending);
        }
    }
}

/// Keep one capture session alive per online device, restarting a session when
/// its device's plan changes, and dispatch incoming inputs against the plan of
/// the device they arrived on. Runs for the lifetime of the process.
async fn manage(
    capture_plans: SharedCapturePlans,
    capture_plan_changed: Arc<Notify>,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    channel_registry: openlogi_hid::ChannelRegistry,
    outputs: GestureOutputs,
) {
    let (events, mut event_rx) = mpsc::unbounded_channel::<SessionEvent>();
    let mut sessions: HashMap<PhysicalDeviceKey, RunningSession> = HashMap::new();
    let mut pending_restores: HashMap<PhysicalDeviceKey, PendingCaptureRestore> = HashMap::new();
    let mut ticker = tokio::time::interval(TARGET_POLL);
    let mut input_dispatcher = InputDispatcher::new(outputs);
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed. Each session reports its completion here, tagged with its device
    // key and the epoch it started under: a dead *current* session re-arms on the
    // next tick, a deliberately stopped one merely frees its key for the
    // replacement once its teardown has drained, and stale completions are
    // ignored by the shared capture-session lifecycle.
    let channels = SessionChannels {
        events,
        capture: capture_channel,
        registry: channel_registry,
    };
    // The capture-vs-pairing arbiter hands out one exclusive lease. All session
    // tasks share it through an `Arc`; the manager keeps only a `Weak` so the
    // lease frees itself when the last session exits (letting pairing proceed).
    let mut lease: std::sync::Weak<SessionReceiverLease> = std::sync::Weak::new();

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    SessionEvent::Input(event) => {
                        let key = &event.physical_key;
                        if let Some(session) = sessions.get_mut(key) {
                            reconcile_published_session(
                                key,
                                session,
                                &receiver_access,
                                &capture_plans,
                                &mut input_dispatcher,
                            );
                        }
                        let live = sessions.get(key);
                        let dispatch_context = dispatch_context_for(&event.session, live);
                        if let Some((session, plan)) = dispatch_context {
                            input_dispatcher.dispatch(session, plan, event.input);
                        } else {
                            input_dispatcher.cancel_session(&event.session);
                            debug!(key = key.as_str(), epoch = event.session.epoch(), "input from a stale capture session — ignored");
                        }
                    }
                    SessionEvent::Done(done) => {
                        let key = &done.physical_key;
                        // Completion is queued behind every input the listener
                        // accepted during restoration, so cancellation cannot
                        // overtake the last diverted edge.
                        if let Some((CompletionAction::Remove { unexpected }, dispatch_session)) =
                            sessions.get(key).map(|session| {
                                (session.completion(&done.session), session.id().clone())
                            })
                        {
                            if let Some(pending) = done.pending_restore {
                                pending_restores.insert(key.clone(), pending);
                            }
                            input_dispatcher.cancel_session(&dispatch_session);
                            if unexpected {
                                warn!(key = key.as_str(), "capture session ended unexpectedly, re-arming");
                            }
                            sessions.remove(key);
                        }
                    }
                }
            }
            () = wait_for_reconcile(&mut ticker, &capture_plan_changed) => {
                // While pairing is waiting or active, release every capture
                // session so run_pairing can own the receiver's HID node (one
                // process can't read it through two channels).
                let want = wanted_sessions(&receiver_access, &capture_plans);
                // Stop sessions whose device disappeared or whose plan changed.
                // Sending on the oneshot lets the session restore its controls.
                // A stopped session stays tracked in the draining phase until
                // its task reports completion below, and a tracked key is never
                // re-armed: arming the replacement while the old task may still
                // be mid-restore could interleave its divert writes with the
                // restore writes on the same device, leaving a control
                // un-diverted while the new session believes it owns it,
                // however many ticks the restore takes. Its lifecycle stays
                // live until completion so already-diverted edges can settle
                // against the retiring plan during teardown.
                for (key, session) in &mut sessions {
                    let wanted = want.get(key).map(|plan| (&plan.target, &plan.dispatch));
                    reconcile_session(session, wanted, &mut input_dispatcher);
                }
                // Firmware ownership outlives the desired plan. Retry every
                // pending restore even when the device was disabled or its
                // plan disappeared, and consume/reinsert the typed token so a
                // successful attempt cannot accidentally be retried.
                // Keep the strong lease through successor spawning below: the
                // restore→rearm transition is one uninterrupted shared-access
                // interval from pairing's perspective.
                let restore_lease = if pending_restores.is_empty() {
                    None
                } else {
                    let Some(restore_lease) = acquire_session_lease(
                        &receiver_access,
                        &mut lease,
                    ) else {
                        continue;
                    };
                    Some(restore_lease)
                };
                if restore_lease.is_some() {
                    retry_pending_restores(&mut pending_restores, &channels.registry).await;
                }
                for (key, plan) in want {
                    if sessions.contains_key(&key) || pending_restores.contains_key(&key) {
                        continue;
                    }
                    // Restoration and capture both touch the receiver channel,
                    // so acquire the shared session lease before either. This
                    // closes the window where pairing could acquire exclusive
                    // access between a pending restore and successor arming.
                    let Some(session_lease) = acquire_session_lease(&receiver_access, &mut lease)
                    else {
                        continue;
                    };
                    let id = HidppSessionId::new(&plan.dispatch.config_key);
                    let session = spawn_session(id, plan, session_lease, &channels);
                    sessions.insert(key, session);
                }
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
    tokio::spawn(async move {
        let _lease = lease;
        let pending_restore = match run_capture_session_with_registry_spec(
            session_route,
            session_spec,
            session_tx,
            stop_rx,
            slot,
            &registry,
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
