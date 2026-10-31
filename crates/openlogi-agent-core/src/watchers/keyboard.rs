//! Background HID++ key-capture watcher, one session per bound keyboard.
//!
//! Runs [`openlogi_hid::run_keyboard_capture_session`] on a dedicated thread
//! for every keyboard-family device the orchestrator publishes in
//! [`SharedKeyboardSpecs`] — an MX Keys and an MX Creative Keypad, or two
//! keypads, capture side by side — restarts a device's session when its route
//! changes, has it follow its set of bound keys in place, and dispatches each
//! captured key press against the bindings of the device it arrived on
//! through the common action path ([`crate::runtime::ActionDispatcher`]).
//!
//! The mouse capture watcher ([`super::gesture`]) and this one hold *shared*
//! receiver leases, so both run concurrently; pairing still waits for (and
//! excludes) both. Like the gesture watcher, this needs no macOS Accessibility
//! permission — the key events arrive over HID++.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};

use openlogi_core::binding::{Binding, ButtonId};
use openlogi_hid::{
    CaptureHost, CaptureSessionOutcome, CaptureSessionStop, CapturedInput, DeviceRoute,
    KeyboardWanted, PendingCaptureRestore, run_keyboard_capture_session,
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use super::capture_manager::{self, CaptureManager, ManagerInputs, PendingRestore};
use super::capture_session::{
    CaptureRecovery, CaptureSession, CaptureSlot, ReconcileAction, settle_capture_task,
};
use super::retry::RETRY_DELAY;
use super::shutdown::{ManagerCompletion, WatcherHandle};
use crate::hardware::DeviceAccess;
use crate::receiver_access::{ReceiverAccess, ReceiverRequestState, SessionReceiverLease};
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Everything the watcher needs to capture one keyboard: where it is, which
/// `0x1b04` controls to divert and which keypad grid keys to observe (only
/// keys carrying a real binding), and the per-key action map presses dispatch
/// through. Rebuilt by the orchestrator on config / inventory /
/// foreground-app changes.
///
/// `wanted` and `grid` are not part of the hardware target. When a per-app
/// profile binds a key the device-wide profile leaves native, the running
/// session diverts (or observes) that key in place as the app comes to the
/// front, and hands it back when the app leaves; the keys that stay diverted
/// are not written (#2140). Bindings that only change an already-captured
/// key's action hot-swap without touching firmware. Only a different route
/// restarts the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardSpec {
    /// Current config namespace for actions from this keyboard. Settings
    /// adoption may change it without cycling an unchanged hardware target.
    pub config_key: String,
    /// HID++ route of the keyboard.
    pub route: DeviceRoute,
    /// `0x1b04` control ID → button, for exactly the bound keys. Empty while
    /// the app in front binds no key of a keyboard that binds keys for other
    /// apps.
    pub wanted: BTreeMap<u16, ButtonId>,
    /// Keypad grid keys → button, keyed by the wire index, for exactly the
    /// bound keys. These change no firmware state.
    pub grid: BTreeMap<u8, ButtonId>,
    /// Effective per-key immediate or threshold map (per-app overlay applied).
    pub bindings: BTreeMap<ButtonId, Binding>,
}

/// Read-only, lossless, coalescing view of the keyboard-capture specs: one per
/// bound keyboard, at most one per route.
pub type SharedKeyboardSpecs = watch::Receiver<Arc<Vec<KeyboardSpec>>>;

/// Capture identity: the keyboard's route. Only a change here restarts the
/// hardware session; everything else is in the [`KeyboardDispatchPlan`].
#[derive(Clone, PartialEq, Eq)]
struct KeyboardTarget {
    route: DeviceRoute,
}

impl KeyboardTarget {
    fn for_spec(spec: &KeyboardSpec) -> Self {
        Self {
            route: spec.route.clone(),
        }
    }
}

/// What a running session applies without a restart: where presses
/// dispatch, and which keys the session keeps captured.
#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyboardDispatchPlan {
    config_key: String,
    bindings: BTreeMap<ButtonId, Binding>,
    /// The `0x1b04` controls to divert and grid keys to observe, for exactly
    /// the bound keys.
    keys: KeyboardWanted,
}
type RunningKeyboardSession =
    CaptureSession<KeyboardTarget, KeyboardDispatchPlan, CaptureSessionStop>;
type KeyboardSlot =
    CaptureSlot<KeyboardTarget, KeyboardDispatchPlan, PendingRestore, CaptureSessionStop>;

struct KeyboardInput {
    route: DeviceRoute,
    session: HidppSessionId,
    input: CapturedInput,
}

struct KeyboardDone {
    route: DeviceRoute,
    session: HidppSessionId,
    pending_restore: Option<PendingCaptureRestore>,
}

enum KeyboardSessionEvent {
    Input(KeyboardInput),
    Done(KeyboardDone),
}

struct KeyboardManagerState {
    slots: HashMap<DeviceRoute, KeyboardSlot>,
    dispatcher: ActionDispatcher,
    lease: Weak<SessionReceiverLease>,
    /// The keys each route's tracked session keeps captured, as its dispatch
    /// plan names them. Written only from that plan; the session reads it.
    keys: RouteKeys,
}

/// One captured-key publication per keyboard route.
type RouteKeys = HashMap<DeviceRoute, watch::Sender<KeyboardWanted>>;

/// The captured-key publication for `route`, created on first use.
fn route_keys<'a>(
    keys: &'a mut RouteKeys,
    route: &DeviceRoute,
) -> &'a watch::Sender<KeyboardWanted> {
    keys.entry(route.clone())
        .or_insert_with(|| watch::Sender::new(KeyboardWanted::default()))
}

struct KeyboardSessionChannels {
    access: DeviceAccess,
    events: mpsc::UnboundedSender<KeyboardSessionEvent>,
}

struct KeyboardManagerContext {
    specs: SharedKeyboardSpecs,
    access: DeviceAccess,
    receiver_requests: watch::Receiver<ReceiverRequestState>,
    dispatcher: ActionDispatcher,
    shutdown: oneshot::Receiver<()>,
}

/// Spawn the keyboard-capture manager thread. It owns a current-thread tokio
/// runtime that keeps one capture session pointed at each bound keyboard and
/// dispatches each captured key press.
#[must_use]
pub fn spawn(
    specs: &SharedKeyboardSpecs,
    access: DeviceAccess,
    dispatcher: ActionDispatcher,
) -> WatcherHandle {
    let specs = specs.clone();
    let receiver_requests = access.receiver_access.subscribe_requests();
    WatcherHandle::spawn("openlogi-keyboard-watcher", move |shutdown| {
        manage(KeyboardManagerContext {
            specs,
            access,
            receiver_requests,
            dispatcher,
            shutdown,
        })
    })
}

/// Route one accepted keyboard edge through the shared HID++ lifecycle.
fn dispatch_input(
    session: &HidppSessionId,
    input: CapturedInput,
    bindings: &KeyboardDispatchPlan,
    dispatcher: &ActionDispatcher,
) {
    match input {
        CapturedInput::ButtonDown(button) => {
            let binding = bindings.bindings.get(&button);
            if let Some(binding) = binding {
                info!(button = %button, action = %binding.click_action().label(), "keyboard key → handling binding");
            } else {
                debug!(?button, "keyboard key with no binding — ignored");
            }
            dispatcher.try_hidpp_button_down(session, button, binding, None);
        }
        CapturedInput::ButtonUp(button) => {
            dispatcher.try_hidpp_button_up(session, button);
        }
        CapturedInput::ButtonPulse(button) => {
            dispatcher.dispatch_hidpp_button_pulse(
                session,
                button,
                bindings.bindings.get(&button),
                None,
            );
        }
        CapturedInput::Interrupted => dispatcher.cancel_hidpp_session(session),
        CapturedInput::Gesture(..)
        | CapturedInput::Scroll { .. }
        | CapturedInput::ThumbwheelDirection { .. }
        | CapturedInput::ButtonDiverted(_)
        | CapturedInput::HeldWheel { .. }
        | CapturedInput::MainWheel { .. }
        | CapturedInput::HostModeEntered => {}
    }
}

/// The capture target and dispatch plan of every published keyboard, or none
/// while pairing owns capture.
fn wanted_sessions(
    requests: ReceiverRequestState,
    specs: &[KeyboardSpec],
) -> Vec<(KeyboardTarget, KeyboardDispatchPlan)> {
    if requests.any() {
        return Vec::new();
    }
    specs
        .iter()
        .map(|spec| {
            (
                KeyboardTarget::for_spec(spec),
                KeyboardDispatchPlan {
                    config_key: spec.config_key.clone(),
                    bindings: spec.bindings.clone(),
                    keys: KeyboardWanted {
                        controls: spec.wanted.clone(),
                        grid: spec.grid.clone(),
                    },
                },
            )
        })
        .collect()
}

/// The entry of `wanted` for the keyboard at `route`.
fn wanted_for<'a>(
    wanted: &'a [(KeyboardTarget, KeyboardDispatchPlan)],
    route: &DeviceRoute,
) -> Option<(&'a KeyboardTarget, &'a KeyboardDispatchPlan)> {
    wanted
        .iter()
        .find(|(target, _)| target.route == *route)
        .map(|(target, dispatch)| (target, dispatch))
}

/// Bring the session to `wanted`. A new plan hot-swaps, and its key set
/// reaches the running session through `keys`, which diverts and hands back
/// keys in place.
fn reconcile_session(
    running: &mut RunningKeyboardSession,
    wanted: Option<(&KeyboardTarget, &KeyboardDispatchPlan)>,
    dispatcher: &ActionDispatcher,
    keys: &watch::Sender<KeyboardWanted>,
) {
    let action = running.reconcile_with(wanted, |_, _| CaptureSessionStop::Shutdown);
    if action != ReconcileAction::None {
        dispatcher.cancel_hidpp_session(running.id());
    }
    if action == ReconcileAction::DispatchChanged {
        let config_key = running.dispatch().config_key.clone();
        running.rekey(&config_key);
        let wanted = &running.dispatch().keys;
        keys.send_if_modified(|current| {
            let changed = current != wanted;
            if changed {
                current.clone_from(wanted);
            }
            changed
        });
    }
}

/// Reconcile the session at `route` directly against the latest publication.
/// Input calls this before dispatch so an event cannot slip between
/// publishing a hot binding update and processing its notification.
fn reconcile_published_session(
    route: &DeviceRoute,
    running: &mut RunningKeyboardSession,
    receiver_requests: &watch::Receiver<ReceiverRequestState>,
    specs: &SharedKeyboardSpecs,
    dispatcher: &ActionDispatcher,
    keys: &watch::Sender<KeyboardWanted>,
) {
    let wanted = wanted_sessions(*receiver_requests.borrow(), &specs.borrow());
    reconcile_session(running, wanted_for(&wanted, route), dispatcher, keys);
}

/// The shared receiver lease every keyboard session holds, acquiring it when
/// none is alive.
fn acquire_session_lease(
    receiver_access: &ReceiverAccess,
    lease: &mut Weak<SessionReceiverLease>,
) -> Option<Arc<SessionReceiverLease>> {
    if let Some(existing) = lease.upgrade() {
        return Some(existing);
    }
    let fresh = Arc::new(receiver_access.try_acquire_for_session()?);
    *lease = Arc::downgrade(&fresh);
    Some(fresh)
}

/// Retry every restore owed and due by `now`, each on its own route.
async fn retry_pending_restores(
    slots: &mut HashMap<DeviceRoute, KeyboardSlot>,
    registry: &openlogi_hid::ChannelRegistry,
    now: tokio::time::Instant,
) {
    let due: Vec<_> = slots
        .iter()
        .filter(|(_, slot)| {
            slot.recovery()
                .and_then(|recovery| recovery.pending_restore.as_ref())
                .is_some_and(|pending| pending.retry_at <= now)
        })
        .map(|(route, _)| route.clone())
        .collect();
    for route in due {
        let Some(KeyboardSlot::Recovering(mut recovery)) = slots.remove(&route) else {
            continue;
        };
        let Some(pending) = recovery.pending_restore.take() else {
            slots.insert(route, KeyboardSlot::Recovering(recovery));
            continue;
        };
        if let CaptureSessionOutcome::RestorePending(token) = pending.token.retry(registry).await {
            recovery.pending_restore = Some(PendingRestore {
                token,
                retry_at: tokio::time::Instant::now() + RETRY_DELAY,
            });
        }
        if !recovery.is_empty() {
            slots.insert(route, KeyboardSlot::Recovering(recovery));
        }
    }
}

/// The earliest instant recovery work in `slots` can make progress, or
/// `None` while an exclusive request or suspended device I/O parks it.
fn next_deadline(
    requests: ReceiverRequestState,
    device_io_allowed: bool,
    slots: &HashMap<DeviceRoute, KeyboardSlot>,
) -> Option<tokio::time::Instant> {
    if requests.any() || !device_io_allowed {
        return None;
    }
    slots
        .values()
        .filter_map(KeyboardSlot::recovery)
        .filter_map(|recovery| recovery.next_deadline(|pending| pending.retry_at))
        .min()
}

impl KeyboardManagerState {
    fn new(dispatcher: ActionDispatcher) -> Self {
        Self {
            slots: HashMap::new(),
            dispatcher,
            lease: Weak::new(),
            keys: HashMap::new(),
        }
    }

    fn deadline(
        &self,
        requests: ReceiverRequestState,
        device_io_allowed: bool,
    ) -> Option<tokio::time::Instant> {
        next_deadline(requests, device_io_allowed, &self.slots)
    }

    fn expedite_pending_restores(&mut self) {
        let now = tokio::time::Instant::now();
        for slot in self.slots.values_mut() {
            if let Some(pending) = slot
                .recovery_mut()
                .and_then(|recovery| recovery.pending_restore.as_mut())
            {
                pending.retry_at = now;
            }
        }
    }

    /// Drop the key publication of every route with no slot left: a later
    /// session on that route starts from its own plan.
    fn forget_idle_routes(&mut self) {
        let slots = &self.slots;
        self.keys.retain(|route, _| slots.contains_key(route));
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
        published: &[KeyboardSpec],
        channels: &KeyboardSessionChannels,
    ) {
        let receiver_access = &channels.access.receiver_access;
        // Preserve the passive listeners and diverted-key ownership across
        // display sleep. Stopping or retrying them while suspended would issue
        // the same proactive HID writes that can promote macOS DarkWake.
        if !device_io_allowed {
            return;
        }
        let now = tokio::time::Instant::now();
        let wanted = wanted_sessions(requests, published);
        for (route, slot) in &mut self.slots {
            if let Some(running) = slot.session_mut() {
                reconcile_session(
                    running,
                    wanted_for(&wanted, route),
                    &self.dispatcher,
                    route_keys(&mut self.keys, route),
                );
            }
        }
        // A keyboard no longer published needs no restart, but restoration
        // remains mandatory.
        self.slots.retain(|route, slot| {
            let Some(recovery) = slot.recovery_mut() else {
                return true;
            };
            if !published.iter().any(|spec| spec.route == *route) {
                recovery.restart_at = None;
            }
            !recovery.is_empty()
        });
        self.forget_idle_routes();
        if requests.any() {
            return;
        }

        // Keep the strong lease through successor spawning, so a due retry
        // hands its receiver access straight to the re-armed session.
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
            // A keyboard's restore stays on its own route.
            retry_pending_restores(&mut self.slots, &channels.access.registry, now).await;
        }

        for (target, dispatch) in wanted {
            if self
                .slots
                .get(&target.route)
                .is_some_and(|slot| match slot {
                    KeyboardSlot::Running(_) => true,
                    KeyboardSlot::Recovering(recovery) => recovery.blocks_restart(now),
                })
            {
                continue;
            }
            let route = target.route.clone();
            let Some(lease) = acquire_session_lease(receiver_access, &mut self.lease) else {
                self.slots.insert(
                    route,
                    KeyboardSlot::recovering(None, Some(now + RETRY_DELAY)),
                );
                continue;
            };
            let keys = route_keys(&mut self.keys, &route);
            let session = spawn_session(target, dispatch, lease, channels, keys);
            self.slots.insert(route, KeyboardSlot::running(session));
        }
    }

    fn handle_session_event(
        &mut self,
        event: KeyboardSessionEvent,
        device_io_allowed: bool,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        specs: &SharedKeyboardSpecs,
    ) -> bool {
        match event {
            KeyboardSessionEvent::Input(input) => {
                let route = &input.route;
                if device_io_allowed
                    && let Some(running) = self
                        .slots
                        .get_mut(route)
                        .and_then(KeyboardSlot::session_mut)
                {
                    reconcile_published_session(
                        route,
                        running,
                        receiver_requests,
                        specs,
                        &self.dispatcher,
                        route_keys(&mut self.keys, route),
                    );
                }

                let Some(running) = self
                    .slots
                    .get(route)
                    .and_then(KeyboardSlot::session)
                    .filter(|running| running.owns(&input.session))
                else {
                    self.dispatcher.cancel_hidpp_session(&input.session);
                    debug!(
                        epoch = input.session.epoch(),
                        "input from a stale keyboard session — ignored"
                    );
                    return false;
                };
                dispatch_input(
                    running.id(),
                    input.input,
                    running.dispatch(),
                    &self.dispatcher,
                );
                false
            }
            KeyboardSessionEvent::Done(done) => {
                // Input and Done share this queue, and the forwarding task is
                // drained before Done is sent. A tracked draining session
                // therefore remains the sole input owner until firmware
                // restoration is complete.
                let now = tokio::time::Instant::now();
                let pending_restore = done.pending_restore.map(|token| PendingRestore {
                    token,
                    retry_at: now + RETRY_DELAY,
                });
                let restart_at = device_io_allowed.then_some(now + RETRY_DELAY);
                let Some(slot) = self.slots.get_mut(&done.route) else {
                    return false;
                };
                let Some((dispatch_session, unexpected)) =
                    slot.complete(&done.session, pending_restore, restart_at)
                else {
                    return false;
                };
                let recovery_finished = slot.recovery().is_some_and(CaptureRecovery::is_empty);
                self.dispatcher.cancel_hidpp_session(&dispatch_session);
                if unexpected && device_io_allowed {
                    warn!(
                        route = ?done.route,
                        "keyboard capture session ended unexpectedly, delaying re-arm"
                    );
                }
                if recovery_finished {
                    self.slots.remove(&done.route);
                    self.forget_idle_routes();
                }
                true
            }
        }
    }
}

/// Retire every tracked keyboard epoch, preserve each one's ordered input
/// ownership until completion, then retain and retry any firmware restore
/// token until ownership has been released.
async fn drain_for_shutdown(
    state: &mut KeyboardManagerState,
    event_rx: &mut mpsc::UnboundedReceiver<KeyboardSessionEvent>,
    receiver_requests: &watch::Receiver<ReceiverRequestState>,
    specs: &SharedKeyboardSpecs,
    channels: &KeyboardSessionChannels,
) {
    for (route, slot) in &mut state.slots {
        if let Some(running) = slot.session_mut() {
            reconcile_session(
                running,
                None,
                &state.dispatcher,
                route_keys(&mut state.keys, route),
            );
        }
    }
    while state.slots.values().any(|slot| slot.session().is_some()) {
        let Some(event) = event_rx.recv().await else {
            break;
        };
        state.handle_session_event(
            event,
            channels.access.device_io.allows_io(),
            receiver_requests,
            specs,
        );
    }

    if state.has_pending_restores() {
        warn!("keyboard watcher is waiting for pending firmware restoration before stopping");
    }
    let mut device_io = channels.access.device_io.clone();
    while state.has_pending_restores() {
        if !device_io.allows_io() && !device_io.wait_until_allowed().await {
            // A closed suspended gate cannot prove firmware is native. The
            // process lifecycle bounds terminal exits; confirmed replacement
            // deliberately remains here rather than losing these tokens.
            std::future::pending::<()>().await;
        }
        state.expedite_pending_restores();
        let requests = *receiver_requests.borrow();
        state.reconcile(requests, true, &[], channels).await;
        if state.has_pending_restores() {
            tokio::time::sleep(RETRY_DELAY).await;
        }
    }
}

/// The keyboard manager as the shared loop drives it: one slot per keyboard
/// route, and one weak receiver lease shared by every session.
struct KeyboardManager {
    state: KeyboardManagerState,
    channels: KeyboardSessionChannels,
}

impl CaptureManager for KeyboardManager {
    type Published = Arc<Vec<KeyboardSpec>>;
    type Event = KeyboardSessionEvent;

    async fn reconcile(&mut self, requests: ReceiverRequestState, published: &Self::Published) {
        self.state
            .reconcile(requests, true, published, &self.channels)
            .await;
    }

    fn handle_session_event(
        &mut self,
        event: KeyboardSessionEvent,
        device_io_allowed: bool,
        receiver_requests: &watch::Receiver<ReceiverRequestState>,
        published: &watch::Receiver<Self::Published>,
    ) -> bool {
        self.state
            .handle_session_event(event, device_io_allowed, receiver_requests, published)
    }

    fn deadline(
        &self,
        requests: ReceiverRequestState,
        device_io_allowed: bool,
    ) -> Option<tokio::time::Instant> {
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
        events: &mut mpsc::UnboundedReceiver<KeyboardSessionEvent>,
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

/// Keep one keyboard capture session alive per published spec, restarting a
/// keyboard's session when its route changes, and dispatch incoming presses.
/// Runs for the lifetime of the process.
async fn manage(context: KeyboardManagerContext) -> ManagerCompletion {
    let KeyboardManagerContext {
        specs,
        access,
        receiver_requests,
        dispatcher,
        shutdown,
    } = context;
    let (events, event_rx) = mpsc::unbounded_channel::<KeyboardSessionEvent>();
    let registry_changes = access.registry.subscribe();
    let device_io = access.device_io.clone();
    let channels = KeyboardSessionChannels { access, events };
    capture_manager::run(
        KeyboardManager {
            state: KeyboardManagerState::new(dispatcher),
            channels,
        },
        ManagerInputs {
            published: specs,
            receiver_requests,
            registry_changes,
            device_io,
            events: event_rx,
            shutdown,
        },
    )
    .await
}

/// Start a session for `target`. It arms the keys `dispatch` names and then
/// follows `keys`, which the manager updates with the session's plan.
fn spawn_session(
    target: KeyboardTarget,
    dispatch: KeyboardDispatchPlan,
    receiver_lease: Arc<SessionReceiverLease>,
    channels: &KeyboardSessionChannels,
    keys: &watch::Sender<KeyboardWanted>,
) -> RunningKeyboardSession {
    let (stop_tx, stop_rx) = oneshot::channel();
    let slot = Arc::clone(&channels.access.channel);
    let session_registry = channels.access.registry.clone();
    let id = HidppSessionId::new(&dispatch.config_key);
    let (sink, session_rx) = mpsc::unbounded_channel();
    let forward = spawn_input_forwarder(
        target.route.clone(),
        id.clone(),
        session_rx,
        channels.events.clone(),
    );
    let session_events = channels.events.clone();
    let done_id = id.clone();
    let route = target.route.clone();
    keys.send_replace(dispatch.keys.clone());
    let wanted = keys.subscribe();
    let device_io = channels.access.device_io.clone();
    // The session body runs in a task of its own so that even a panic in it
    // still reaches the completion report below (`settle_capture_task`).
    let session_task = tokio::spawn(async move {
        let _receiver_lease = receiver_lease;
        match run_keyboard_capture_session(
            route,
            wanted,
            CaptureHost {
                sink,
                shutdown: stop_rx,
                channel_slot: slot,
                registry: &session_registry,
                device_io,
            },
        )
        .await
        {
            Ok(CaptureSessionOutcome::Restored) => None,
            Ok(CaptureSessionOutcome::RestorePending(pending)) => Some(pending),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                debug!(%error, "keyboard capture session ended");
                pending
            }
        }
    });
    tokio::spawn(complete_session(
        session_task,
        forward,
        session_events,
        target.route.clone(),
        done_id,
    ));
    CaptureSession::active(id, target, dispatch, stop_tx)
}

/// Forward one capture session's inputs onto the manager's ordered event
/// channel, tagged with the route they arrived on. The sender closes only
/// after the device listener has been dropped.
fn spawn_input_forwarder(
    route: DeviceRoute,
    session: HidppSessionId,
    mut inputs: mpsc::UnboundedReceiver<CapturedInput>,
    events: mpsc::UnboundedSender<KeyboardSessionEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(input) = inputs.recv().await {
            let _ = events.send(KeyboardSessionEvent::Input(KeyboardInput {
                route: route.clone(),
                session: session.clone(),
                input,
            }));
        }
    })
}

/// Settle the session task, then report its completion once every input it
/// accepted has been forwarded. Apart from [`spawn_session`] so the path a
/// panicking body takes can be driven in a test without a device.
async fn complete_session(
    session_task: tokio::task::JoinHandle<Option<PendingCaptureRestore>>,
    forward: tokio::task::JoinHandle<()>,
    events: mpsc::UnboundedSender<KeyboardSessionEvent>,
    route: DeviceRoute,
    session: HidppSessionId,
) {
    let pending_restore = settle_capture_task(session_task, "keyboard").await;
    // The device layer drops its listener only after restoration. Draining
    // the forwarder before Done preserves every input accepted while
    // diversion was still active ahead of the ownership boundary.
    let _ = forward.await;
    let _ = events.send(KeyboardSessionEvent::Done(KeyboardDone {
        route,
        session,
        pending_restore,
    }));
}

#[cfg(test)]
mod tests;
