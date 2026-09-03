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
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use openlogi_core::config::TouchpadScrollSensitivity;
use openlogi_core::device_order::PhysicalDeviceKey;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::session::gesture::{CaptureSessionMode, TouchpadJournalStore};
use openlogi_hid::{
    CaptureChannel, CaptureSessionOutcome, CaptureSessionRequest, CapturedInput, DeviceIoGate,
    FileTouchpadJournalStore, GestureError, PendingCaptureRestore,
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
use crate::touchpad_monitor::SharedTouchpadMonitor;

const RETRY_DELAY: Duration = Duration::from_secs(1);
/// Bounds deliberate process shutdown while active sessions restore their
/// device controls. A timed-out worker remains detached, and the durable raw
/// mode journal lets the next launch recover it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Synthesize one frame of two-finger scrolling from a micrometre centroid
/// delta. The capture session owns the pad's raw stream, which switches its
/// firmware scroll translation off — OpenLogi restores the scrolling itself,
/// the contract Options+ keeps on the same hardware.
///
/// Every frame is wheel-class — no scroll phase, matching what the pad's
/// firmware emits natively. Phased frames carry drag semantics: apps
/// rubber-band them at document boundaries and keep the page stretched while
/// later deltas feed the overscroll. Wheel-class deltas clamp, exactly the
/// native feel this device has without capture.
fn post_touchpad_scroll(tuning: TouchpadScrollTuning, dx: i64, dy: i64) {
    openlogi_inject::post_touchpad_scroll(tuning.content_delta(dx, dy));
}

/// Effective tuning of one device's synthesized two-finger scrolling.
#[derive(Clone, Copy, Debug)]
pub(super) struct TouchpadScrollTuning {
    sensitivity: TouchpadScrollSensitivity,
    inverted: bool,
}

impl TouchpadScrollTuning {
    /// Project the plan's per-device settings.
    pub(super) fn from_plan(plan: &DispatchPlan) -> Self {
        Self {
            sensitivity: plan.touchpad_scroll_sensitivity,
            inverted: plan.touchpad_scroll_inverted,
        }
    }

    /// Convert one centroid delta into a content-following pixel scroll.
    ///
    /// Fingers right / down move content right / down (natural scrolling).
    /// ScrollDelta's wheel convention is +x view-right / +y view-up, so
    /// content-following maps the horizontal axis negated and the vertical
    /// axis as-is; the inject layer re-orients for hosts whose wheel
    /// convention matches content-following instead. `inverted` flips both
    /// axes after that, mirroring the wheel contract of running opposite the
    /// system scroll preference.
    pub(super) fn content_delta(self, dx: i64, dy: i64) -> ScrollDelta {
        let (dx, dy) = if self.inverted { (-dx, -dy) } else { (dx, dy) };
        let multiplier = self.sensitivity.scroll_multiplier();
        ScrollDelta::pixels(
            micrometres_to_content_pixels(-dx) * multiplier,
            micrometres_to_content_pixels(dy) * multiplier,
        )
    }
}

/// Two-finger scroll gain in pixels of content per micrometre of centroid
/// travel: 25 px/mm keeps the Casa Touch's 75 mm-tall surface good for a
/// ~1.9k px full-height stroke, the distance a Magic Trackpad covers at its
/// default tracking feel.
const TOUCHPAD_SCROLL_PIXELS_PER_UM: f64 = 0.025;

#[expect(
    clippy::cast_precision_loss,
    reason = "micrometre deltas from a 117 x 76 mm pad stay far below 2^53"
)]
fn micrometres_to_content_pixels(um: i64) -> f64 {
    um as f64 * TOUCHPAD_SCROLL_PIXELS_PER_UM
}

/// Unique owner of the capture-manager thread and its graceful shutdown.
pub struct GestureWatcher {
    shutdown: Option<oneshot::Sender<()>>,
    done: std_mpsc::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl GestureWatcher {
    /// Stop every capture session, wait for its control restoration, and join
    /// the manager thread. Returns false when the bounded wait expires or the
    /// worker panicked.
    pub fn shutdown(&mut self) -> bool {
        self.shutdown_with_timeout(SHUTDOWN_TIMEOUT)
    }

    fn shutdown_with_timeout(&mut self, timeout: Duration) -> bool {
        let Some(worker) = self.worker.take() else {
            return true;
        };
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if self.done.recv_timeout(timeout).is_err() {
            warn!("capture watcher did not restore every session before the shutdown deadline");
            return false;
        }
        if worker.join().is_err() {
            warn!("capture watcher panicked during shutdown");
            return false;
        }
        true
    }
}

impl Drop for GestureWatcher {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Spawn the capture-manager thread. It owns a current-thread tokio runtime
/// that keeps one capture session pointed at the active device and dispatches
/// each captured input. The returned owner must be shut down before process
/// exit so raw touchpad mode can be restored.
#[must_use]
pub fn spawn(
    capture_plans: &SharedCapturePlans,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
    outputs: GestureOutputs,
    touchpad_monitor: SharedTouchpadMonitor,
    channels: (
        CaptureChannel,
        openlogi_hid::ChannelRegistry,
        Arc<std::sync::atomic::AtomicBool>,
    ),
) -> GestureWatcher {
    let (capture_channel, channel_registry, native_button) = channels;
    let plans = capture_plans.clone();
    let receiver_requests = receiver_access.subscribe_requests();
    let (shutdown, shutdown_rx) = oneshot::channel();
    let (done, wait) = std_mpsc::channel();
    let worker = thread::Builder::new()
        .name("openlogi-capture".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    warn!(error = %e, "capture watcher: could not build tokio runtime");
                    let _ = done.send(());
                    return;
                }
            };
            let (context, event_rx) = ManagerContext::new(
                plans,
                receiver_access,
                receiver_requests,
                touchpad_monitor,
                (capture_channel, channel_registry, device_io, native_button),
            );
            runtime.block_on(manage(context, event_rx, outputs, shutdown_rx));
            let _ = done.send(());
        });
    match worker {
        Ok(worker) => GestureWatcher {
            shutdown: Some(shutdown),
            done: wait,
            worker: Some(worker),
        },
        Err(error) => {
            warn!(%error, "capture watcher: could not spawn manager thread");
            GestureWatcher {
                shutdown: None,
                done: wait,
                worker: None,
            }
        }
    }
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
    error: Option<GestureError>,
}

enum SessionEvent {
    Input(CapturedEvent),
    Done(SessionDone),
}

struct PendingRestore {
    token: PendingCaptureRestore,
    retry_at: Instant,
}

#[derive(Clone)]
struct BlockedTouchpad {
    target: CaptureTarget,
    config_key: String,
}

struct GestureManagerState {
    sessions: HashMap<PhysicalDeviceKey, RunningSession>,
    pending_restores: HashMap<PhysicalDeviceKey, PendingRestore>,
    restart_after: HashMap<PhysicalDeviceKey, Instant>,
    blocked_touchpads: HashMap<PhysicalDeviceKey, BlockedTouchpad>,
    input_dispatcher: InputDispatcher,
    lease: std::sync::Weak<SessionReceiverLease>,
}

#[derive(Clone)]
struct SessionChannels {
    events: mpsc::UnboundedSender<SessionEvent>,
    capture: CaptureChannel,
    registry: openlogi_hid::ChannelRegistry,
    device_io: DeviceIoGate,
    touchpad_journal: Option<Arc<dyn TouchpadJournalStore>>,
    native_button: Arc<std::sync::atomic::AtomicBool>,
}

struct ManagerContext {
    capture_plans: watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
    receiver_access: ReceiverAccess,
    receiver_requests: watch::Receiver<ReceiverRequestState>,
    channels: SessionChannels,
    device_io: DeviceIoGate,
    touchpad_monitor: SharedTouchpadMonitor,
}

impl ManagerContext {
    fn new(
        capture_plans: watch::Receiver<Arc<Vec<DeviceCapturePlan>>>,
        receiver_access: ReceiverAccess,
        receiver_requests: watch::Receiver<ReceiverRequestState>,
        touchpad_monitor: SharedTouchpadMonitor,
        channels: (
            CaptureChannel,
            openlogi_hid::ChannelRegistry,
            DeviceIoGate,
            Arc<std::sync::atomic::AtomicBool>,
        ),
    ) -> (Self, mpsc::UnboundedReceiver<SessionEvent>) {
        let (capture_channel, channel_registry, device_io, native_button) = channels;
        let (events, event_rx) = mpsc::unbounded_channel();
        let touchpad_journal = match FileTouchpadJournalStore::in_state_dir() {
            Ok(store) => Some(Arc::new(store) as Arc<dyn TouchpadJournalStore>),
            Err(error) => {
                warn!(error = %error, "touchpad raw-mode journal unavailable — raw capture disabled");
                None
            }
        };
        (
            Self {
                capture_plans,
                receiver_access,
                receiver_requests,
                channels: SessionChannels {
                    events,
                    capture: capture_channel,
                    registry: channel_registry,
                    device_io: device_io.clone(),
                    touchpad_journal,
                    native_button: native_button.clone(),
                },
                device_io,
                touchpad_monitor,
            },
            event_rx,
        )
    }
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
fn wanted_sessions(
    requests: ReceiverRequestState,
    published: &Arc<Vec<DeviceCapturePlan>>,
    touchpad_monitor: &SharedTouchpadMonitor,
    touchpad_journal: Option<&dyn TouchpadJournalStore>,
) -> Arc<Vec<DeviceCapturePlan>> {
    if requests.any() {
        return Arc::new(Vec::new());
    }
    Arc::new(
        published
            .iter()
            .filter_map(|plan| effective_plan(plan, touchpad_monitor, touchpad_journal))
            .collect(),
    )
}

fn effective_plan(
    plan: &DeviceCapturePlan,
    touchpad_monitor: &SharedTouchpadMonitor,
    touchpad_journal: Option<&dyn TouchpadJournalStore>,
) -> Option<DeviceCapturePlan> {
    let mut plan = plan.clone();
    let diagnostic = touchpad_monitor.capture_requested_for(&plan.dispatch.config_key)
        && plan.target.spec.touchpad_journal_id.is_some();
    if diagnostic {
        plan.target.spec.capture_touchpad = true;
        if plan.target.spec.mode == CaptureSessionMode::TouchpadRecovery {
            plan.target.spec.mode = CaptureSessionMode::TouchpadOnly;
        }
    }
    if plan.target.spec.capture_touchpad && touchpad_journal.is_none() {
        return None;
    }
    if plan.target.spec.mode == CaptureSessionMode::TouchpadRecovery {
        let journal_id = plan.target.spec.touchpad_journal_id.as_deref()?;
        let journal = touchpad_journal?;
        match journal.load(journal_id) {
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(error) => warn!(
                key = plan.dispatch.config_key,
                %error,
                "could not inspect touchpad raw-mode journal — attempting recovery"
            ),
        }
    }
    Some(plan)
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
    touchpad_monitor: &SharedTouchpadMonitor,
    touchpad_journal: Option<&dyn TouchpadJournalStore>,
    dispatcher: &mut InputDispatcher,
) -> bool {
    if receiver_requests.borrow().any() {
        reconcile_session(session, None, dispatcher);
        return false;
    }
    let plans = capture_plans.borrow();
    let Some(published) = plans.iter().find(|plan| plan.target.physical_key == *key) else {
        reconcile_session(session, None, dispatcher);
        return false;
    };
    let actions_enabled = published.target.spec.capture_touchpad;
    let effective = effective_plan(published, touchpad_monitor, touchpad_journal);
    let wanted = effective
        .as_ref()
        .map(|plan| (&plan.target, &plan.dispatch));
    reconcile_session(session, wanted, dispatcher);
    actions_enabled
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

fn request_session_stops(
    sessions: &mut HashMap<PhysicalDeviceKey, RunningSession>,
    mut cancel: impl FnMut(&HidppSessionId),
) {
    for session in sessions.values_mut() {
        cancel(session.id());
        let _ = session.reconcile(None);
    }
}

impl GestureManagerState {
    fn new(outputs: GestureOutputs) -> Self {
        Self {
            sessions: HashMap::new(),
            pending_restores: HashMap::new(),
            restart_after: HashMap::new(),
            blocked_touchpads: HashMap::new(),
            input_dispatcher: InputDispatcher::new(outputs),
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

    fn defer_due_pending_restores(&mut self, now: Instant) {
        for pending in self.pending_restores.values_mut() {
            if pending.retry_at <= now {
                pending.retry_at = now + RETRY_DELAY;
            }
        }
    }

    fn handle_event(&mut self, event: SessionEvent, context: &ManagerContext) -> bool {
        match event {
            SessionEvent::Input(event) => {
                self.handle_input(event, context);
                false
            }
            SessionEvent::Done(done) => self.handle_done(done, context),
        }
    }

    fn handle_input(&mut self, event: CapturedEvent, context: &ManagerContext) {
        let key = &event.physical_key;
        let mut touchpad_actions_enabled = false;
        if context.device_io.allows_io()
            && let Some(session) = self.sessions.get_mut(key)
        {
            touchpad_actions_enabled = reconcile_published_session(
                key,
                session,
                &context.receiver_requests,
                &context.capture_plans,
                &context.touchpad_monitor,
                context.channels.touchpad_journal.as_deref(),
                &mut self.input_dispatcher,
            );
        }
        let live = self.sessions.get(key);
        let dispatch_context = dispatch_context_for(&event.session, live);
        if let Some((session, plan)) = dispatch_context {
            context
                .touchpad_monitor
                .record(session.device_key(), &event.input);
            self.input_dispatcher
                .dispatch(session, plan, event.input, touchpad_actions_enabled);
        } else {
            self.input_dispatcher.cancel_session(&event.session);
            debug!(
                key = key.as_str(),
                epoch = event.session.epoch(),
                "input from a stale capture session — ignored"
            );
        }
    }

    fn handle_done(&mut self, done: SessionDone, context: &ManagerContext) -> bool {
        let key = &done.physical_key;
        // Completion is queued behind every input the listener accepted during
        // restoration, so cancellation cannot overtake the last diverted edge.
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
        let tracked = self.sessions.get(key).map(|session| {
            (
                session.target().clone(),
                session.dispatch().config_key.clone(),
            )
        });
        let conflict = match done.error.as_ref() {
            Some(GestureError::TouchpadRawModeConflict { expected, actual }) => {
                Some((*expected, *actual))
            }
            _ => None,
        };
        let recovered_touchpad = done.error.is_none()
            && tracked.as_ref().is_some_and(|(target, _)| {
                target.spec.mode == CaptureSessionMode::TouchpadRecovery
            });
        if let (Some((expected, actual)), Some((target, config_key))) = (conflict, tracked) {
            self.blocked_touchpads.insert(
                key.clone(),
                BlockedTouchpad {
                    target,
                    config_key: config_key.clone(),
                },
            );
            context
                .touchpad_monitor
                .set_conflict(&config_key, expected, actual);
            warn!(
                key = key.as_str(),
                expected,
                actual,
                "touchpad raw-mode conflict — capture blocked until its plan changes"
            );
        }
        self.input_dispatcher.cancel_session(&dispatch_session);
        if context.device_io.allows_io()
            && let Some(deadline) = restart_deadline(
                unexpected && !recovered_touchpad && conflict.is_none(),
                Instant::now(),
            )
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

    fn begin_shutdown(&mut self) {
        self.restart_after.clear();
        request_session_stops(&mut self.sessions, |session| {
            self.input_dispatcher.cancel_session(session);
        });
    }

    async fn finish_shutdown(
        &mut self,
        event_rx: &mut mpsc::UnboundedReceiver<SessionEvent>,
        context: &mut ManagerContext,
        registry_changes: &mut watch::Receiver<()>,
    ) {
        self.begin_shutdown();
        while !self.sessions.is_empty() || !self.pending_restores.is_empty() {
            let now = Instant::now();
            let device_io_allowed = context.device_io.allows_io();
            let restore_due = device_io_allowed
                && self
                    .pending_restores
                    .values()
                    .any(|pending| pending.retry_at <= now);
            if restore_due {
                if acquire_session_lease(&context.receiver_access, &mut self.lease).is_some() {
                    retry_pending_restores(
                        &mut self.pending_restores,
                        &context.channels.registry,
                        now,
                    )
                    .await;
                } else {
                    self.defer_due_pending_restores(now);
                }
                if self.sessions.is_empty() && self.pending_restores.is_empty() {
                    break;
                }
            }

            let deadline = device_io_allowed
                .then(|| {
                    self.pending_restores
                        .values()
                        .map(|pending| pending.retry_at)
                        .min()
                })
                .flatten();
            tokio::select! {
                Some(event) = event_rx.recv() => {
                    if self.handle_event(event, context) {
                        self.expedite_pending_restores();
                    }
                }
                open = wait_for_registry_change(registry_changes, !self.pending_restores.is_empty()) => {
                    if !open {
                        return;
                    }
                    if context.device_io.allows_io() {
                        self.expedite_pending_restores();
                    }
                }
                result = context.receiver_requests.changed() => {
                    if result.is_err() {
                        return;
                    }
                    if context.device_io.allows_io() {
                        self.expedite_pending_restores();
                    }
                }
                allowed = context.device_io.changed() => match allowed {
                    Some(true) => self.expedite_pending_restores(),
                    Some(false) => {}
                    None => return,
                },
                () = wait_for_deadline(deadline) => {}
            }
        }
    }

    async fn reconcile(
        &mut self,
        requests: ReceiverRequestState,
        device_io_allowed: bool,
        published: &Arc<Vec<DeviceCapturePlan>>,
        receiver_access: &ReceiverAccess,
        channels: &SessionChannels,
        touchpad_monitor: &SharedTouchpadMonitor,
    ) {
        // Keep existing passive listeners and their firmware ownership intact
        // while the display/session is asleep. Retiring them here would issue
        // restoration writes during DarkWake; retries and successors wait for
        // the user-visible resume instead.
        if !device_io_allowed {
            return;
        }
        let now = Instant::now();
        let wanted = wanted_sessions(
            requests,
            published,
            touchpad_monitor,
            channels.touchpad_journal.as_deref(),
        );
        for (key, session) in &mut self.sessions {
            let wanted = wanted
                .iter()
                .find(|plan| plan.target.physical_key == *key)
                .map(|plan| (&plan.target, &plan.dispatch));
            reconcile_session(session, wanted, &mut self.input_dispatcher);
        }
        self.restart_after
            .retain(|key, _| wanted.iter().any(|plan| plan.target.physical_key == *key));
        self.blocked_touchpads.retain(|key, blocked| {
            let unchanged = wanted
                .iter()
                .any(|plan| plan.target.physical_key == *key && plan.target == blocked.target);
            if !unchanged {
                touchpad_monitor.clear_conflict(&blocked.config_key);
            }
            unchanged
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

        for plan in wanted.iter() {
            let key = &plan.target.physical_key;
            if self.sessions.contains_key(key)
                || self.pending_restores.contains_key(key)
                || self
                    .blocked_touchpads
                    .get(key)
                    .is_some_and(|blocked| blocked.target == plan.target)
            {
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
}

/// Keep one capture session alive per online device, restarting a session when
/// its device's plan changes, and dispatch incoming inputs against the plan of
/// the device they arrived on. Runs for the lifetime of the process.
async fn manage(
    mut context: ManagerContext,
    mut event_rx: mpsc::UnboundedReceiver<SessionEvent>,
    outputs: GestureOutputs,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut registry_changes = context.channels.registry.subscribe();
    let mut touchpad_requests = context.touchpad_monitor.subscribe_capture_requests();
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed. Each session reports its completion here, tagged with its device
    // key and the epoch it started under: a dead *current* session re-arms on the
    // retry deadline, a deliberately stopped one immediately frees its key for the
    // replacement once its teardown has drained, and stale completions are
    // ignored by the shared capture-session lifecycle.
    let mut state = GestureManagerState::new(outputs);
    let mut reconcile = true;

    loop {
        if reconcile {
            reconcile = false;
            let device_io_allowed = context.device_io.allows_io();
            if device_io_allowed {
                let requests = *context.receiver_requests.borrow_and_update();
                let published = Arc::clone(&context.capture_plans.borrow_and_update());
                state
                    .reconcile(
                        requests,
                        device_io_allowed,
                        &published,
                        &context.receiver_access,
                        &context.channels,
                        &context.touchpad_monitor,
                    )
                    .await;
            }
        }

        let requests = *context.receiver_requests.borrow();
        let deadline = state.deadline(requests, context.device_io.allows_io());
        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            reconcile = true;
            continue;
        }

        tokio::select! {
            _ = &mut shutdown => {
                state
                    .finish_shutdown(
                        &mut event_rx,
                        &mut context,
                        &mut registry_changes,
                    )
                    .await;
                return;
            }
            Some(event) = event_rx.recv() => {
                reconcile |= state.handle_event(event, &context);
            }
            result = context.capture_plans.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            result = context.receiver_requests.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            result = touchpad_requests.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            allowed = context.device_io.changed() => match allowed {
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
                if context.device_io.allows_io() {
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
    let touchpad_journal = channels.touchpad_journal.clone();
    let native_button = channels.native_button.clone();
    tokio::spawn(async move {
        let _lease = lease;
        let result = run_capture_session_with_registry_spec(
            session_route,
            CaptureSessionRequest {
                spec: session_spec,
                touchpad_journal,
                sink: session_tx,
                shutdown: stop_rx,
                channel_slot: slot,
                device_io,
                native_button,
            },
            &registry,
        )
        .await;
        let (error, pending_restore) = match result {
            Ok(CaptureSessionOutcome::Restored) => (None, None),
            Ok(CaptureSessionOutcome::RestorePending(pending)) => (None, Some(pending)),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                debug!(%error, "capture session ended");
                (Some(error), pending)
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
                error,
            },
        )
        .await;
    });
    CaptureSession::active(id, target, dispatch, stop_tx)
}

#[cfg(test)]
mod tests;
