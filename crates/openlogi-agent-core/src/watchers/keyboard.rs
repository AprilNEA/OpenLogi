//! Background HID++ key-capture watcher for a bound keyboard.
//!
//! Runs [`openlogi_hid::run_keyboard_capture_session_with_registry`] on a
//! dedicated thread for the keyboard the orchestrator publishes in
//! [`SharedKeyboardSpec`], restarts it when the keyboard (or the set of bound
//! keys) changes, and dispatches each captured key press through the common
//! action path ([`crate::runtime::ActionDispatcher`]).
//!
//! The mouse capture watcher ([`super::gesture`]) and this one hold *shared*
//! receiver leases, so both run concurrently; pairing still waits for (and
//! excludes) both. Like the gesture watcher, this needs no macOS Accessibility
//! permission — the key events arrive over HID++.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use openlogi_core::binding::{Binding, ButtonId};
use openlogi_hid::{
    CaptureChannel, CaptureSessionOutcome, CapturedInput, ChannelRegistry, DeviceRoute,
    PendingCaptureRestore, run_keyboard_capture_session_with_registry,
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use super::capture_session::{CaptureSession, CompletionAction, ReconcileAction};
use crate::receiver_access::{ReceiverAccess, ReceiverRequestState, SessionReceiverLease};
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Everything the watcher needs to capture one keyboard: where it is, which
/// `0x1b04` controls to divert (only keys carrying a real binding), and the
/// per-key action map presses dispatch through. Rebuilt by the orchestrator on
/// config / inventory / foreground-app changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardSpec {
    /// Current config namespace for actions from this keyboard. Settings
    /// adoption may change it without cycling an unchanged hardware target.
    pub config_key: String,
    /// HID++ route of the keyboard.
    pub route: DeviceRoute,
    /// `0x1b04` control ID → button, for exactly the bound keys.
    pub wanted: BTreeMap<u16, ButtonId>,
    /// Effective per-key immediate or threshold map (per-app overlay applied).
    pub bindings: BTreeMap<ButtonId, Binding>,
}

/// Read-only, lossless, coalescing view of the keyboard-capture spec.
pub type SharedKeyboardSpec = watch::Receiver<Option<Arc<KeyboardSpec>>>;

/// Capture identity excluding bindings, which may change without requiring a
/// hardware session restart when the diverted key set stays the same.
#[derive(Clone, PartialEq, Eq)]
struct KeyboardTarget {
    route: DeviceRoute,
    wanted: BTreeMap<u16, ButtonId>,
}

impl KeyboardTarget {
    fn for_spec(spec: &KeyboardSpec) -> Self {
        Self {
            route: spec.route.clone(),
            wanted: spec.wanted.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyboardDispatchPlan {
    config_key: String,
    bindings: BTreeMap<ButtonId, Binding>,
}
type RunningKeyboardSession = CaptureSession<KeyboardTarget, KeyboardDispatchPlan>;

struct KeyboardInput {
    session: HidppSessionId,
    input: CapturedInput,
}

struct KeyboardDone {
    session: HidppSessionId,
    pending_restore: Option<PendingCaptureRestore>,
}

enum KeyboardSessionEvent {
    Input(KeyboardInput),
    Done(KeyboardDone),
}

struct PendingRestore {
    token: PendingCaptureRestore,
    retry_at: tokio::time::Instant,
}

struct KeyboardManagerState {
    current: Option<RunningKeyboardSession>,
    pending_restore: Option<PendingRestore>,
    restart_at: Option<tokio::time::Instant>,
    dispatcher: ActionDispatcher,
}

struct KeyboardSessionChannels {
    capture: CaptureChannel,
    registry: ChannelRegistry,
    events: mpsc::UnboundedSender<KeyboardSessionEvent>,
}

const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Spawn the keyboard-capture manager thread. It owns a current-thread tokio
/// runtime that keeps one capture session pointed at the bound keyboard and
/// dispatches each captured key press.
pub fn spawn(
    spec: &SharedKeyboardSpec,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    dispatcher: ActionDispatcher,
) {
    let spec = spec.clone();
    let receiver_requests = receiver_access.subscribe_requests();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "keyboard watcher: could not build tokio runtime");
                return;
            }
        };
        runtime.block_on(manage(
            spec,
            keyboard_channel,
            receiver_access,
            receiver_requests,
            registry,
            dispatcher,
        ));
    });
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
            dispatcher.try_hidpp_button_down(session, button, binding);
        }
        CapturedInput::ButtonUp(button) => {
            dispatcher.try_hidpp_button_up(session, button);
        }
        CapturedInput::ButtonPulse(button) => {
            dispatcher.dispatch_hidpp_button_pulse(session, button, bindings.bindings.get(&button));
        }
        CapturedInput::Gesture(..) | CapturedInput::Scroll { .. } => {}
    }
}

/// Snapshot the keyboard capture target and dispatch plan unless pairing
/// currently owns capture.
fn wanted_session(
    requests: ReceiverRequestState,
    spec: &watch::Receiver<Option<Arc<KeyboardSpec>>>,
) -> Option<(KeyboardTarget, KeyboardDispatchPlan)> {
    let published = spec.borrow();
    wanted_session_for(requests, published.as_deref())
}

fn wanted_session_for(
    requests: ReceiverRequestState,
    spec: Option<&KeyboardSpec>,
) -> Option<(KeyboardTarget, KeyboardDispatchPlan)> {
    if requests.any() {
        return None;
    }
    spec.map(|spec| {
        (
            KeyboardTarget::for_spec(spec),
            KeyboardDispatchPlan {
                config_key: spec.config_key.clone(),
                bindings: spec.bindings.clone(),
            },
        )
    })
}

fn reconcile_session(
    running: &mut RunningKeyboardSession,
    wanted: Option<&(KeyboardTarget, KeyboardDispatchPlan)>,
    dispatcher: &ActionDispatcher,
) {
    let desired = wanted.map(|(target, dispatch)| (target, dispatch));
    let action = running.reconcile(desired);
    if action != ReconcileAction::None {
        dispatcher.cancel_hidpp_session(running.id());
    }
    if action == ReconcileAction::DispatchChanged {
        let config_key = running.dispatch().config_key.clone();
        running.rekey(&config_key);
    }
}

impl KeyboardManagerState {
    fn new(dispatcher: ActionDispatcher) -> Self {
        Self {
            current: None,
            pending_restore: None,
            restart_at: None,
            dispatcher,
        }
    }

    fn deadline(&self, requests: ReceiverRequestState) -> Option<tokio::time::Instant> {
        if requests.any() {
            return None;
        }
        self.pending_restore
            .as_ref()
            .map(|pending| pending.retry_at)
            .into_iter()
            .chain(self.restart_at)
            .min()
    }

    fn expedite_pending_restore(&mut self) {
        if let Some(pending) = self.pending_restore.as_mut() {
            pending.retry_at = tokio::time::Instant::now();
        }
    }

    async fn reconcile(
        &mut self,
        requests: ReceiverRequestState,
        published: bool,
        wanted: Option<(KeyboardTarget, KeyboardDispatchPlan)>,
        receiver_access: &ReceiverAccess,
        channels: &KeyboardSessionChannels,
    ) {
        let now = tokio::time::Instant::now();
        if !published {
            self.restart_at = None;
        }
        if let Some(running) = self.current.as_mut() {
            reconcile_session(running, wanted.as_ref(), &self.dispatcher);
            return;
        }
        if requests.any() {
            return;
        }

        // Restoration remains mandatory when the spec disappears. A due
        // retry hands its lease directly to a successor.
        let mut handoff_lease = None;
        if self
            .pending_restore
            .as_ref()
            .is_some_and(|pending| pending.retry_at <= now)
            && let Some(lease) = receiver_access.try_acquire_for_session()
            && let Some(pending) = self.pending_restore.take()
        {
            handoff_lease = Some(lease);
            if let CaptureSessionOutcome::RestorePending(token) =
                pending.token.retry(&channels.registry).await
            {
                self.pending_restore = Some(PendingRestore {
                    token,
                    retry_at: tokio::time::Instant::now() + RETRY_DELAY,
                });
            }
        }
        if self.pending_restore.is_some() || self.restart_at.is_some_and(|deadline| deadline > now)
        {
            return;
        }
        let Some((target, dispatch)) = wanted else {
            return;
        };
        let receiver_lease = handoff_lease.or_else(|| receiver_access.try_acquire_for_session());
        if let Some(receiver_lease) = receiver_lease {
            self.restart_at = None;
            self.current = Some(spawn_session(target, dispatch, receiver_lease, channels));
        } else {
            self.restart_at = Some(now + RETRY_DELAY);
        }
    }
}

/// Keep one keyboard capture session alive for the published spec, restarting
/// it when the keyboard or its bound-key set changes, and dispatch incoming
/// presses. Runs for the lifetime of the process.
async fn manage(
    mut spec: watch::Receiver<Option<Arc<KeyboardSpec>>>,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    mut receiver_requests: watch::Receiver<ReceiverRequestState>,
    registry: ChannelRegistry,
    dispatcher: ActionDispatcher,
) {
    let (events, mut event_rx) = mpsc::unbounded_channel::<KeyboardSessionEvent>();
    let mut registry_changes = registry.subscribe();
    let channels = KeyboardSessionChannels {
        capture: keyboard_channel,
        registry,
        events,
    };
    let mut state = KeyboardManagerState::new(dispatcher);
    let mut reconcile = true;

    loop {
        if reconcile {
            reconcile = false;
            let requests = *receiver_requests.borrow_and_update();
            let published = spec.borrow_and_update().clone();
            let want = wanted_session_for(requests, published.as_deref());
            state
                .reconcile(
                    requests,
                    published.is_some(),
                    want,
                    &receiver_access,
                    &channels,
                )
                .await;
        }

        let requests = *receiver_requests.borrow();
        let deadline = state.deadline(requests);
        if deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
            reconcile = true;
            continue;
        }

        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    KeyboardSessionEvent::Input(input) => {
                        let want = wanted_session(*receiver_requests.borrow(), &spec);
                        if let Some(running) = state.current.as_mut() {
                            reconcile_session(running, want.as_ref(), &state.dispatcher);
                        }

                        let Some(running) = state.current
                            .as_ref()
                            .filter(|running| running.owns(&input.session))
                        else {
                            state.dispatcher.cancel_hidpp_session(&input.session);
                            debug!(epoch = input.session.epoch(), "input from a stale keyboard session — ignored");
                            continue;
                        };
                        dispatch_input(
                            running.id(),
                            input.input,
                            running.dispatch(),
                            &state.dispatcher,
                        );
                    }
                    KeyboardSessionEvent::Done(done) => {
                        // Input and Done share this queue, and the forwarding
                        // task is drained before Done is sent. A tracked
                        // draining session therefore remains the sole input
                        // owner until firmware restoration is complete.
                        if let Some((CompletionAction::Remove { unexpected }, dispatch_session)) =
                            state.current.as_ref().map(|running| {
                                (running.completion(&done.session), running.id().clone())
                            })
                        {
                            state.dispatcher.cancel_hidpp_session(&dispatch_session);
                            state.pending_restore = done.pending_restore.map(|token| PendingRestore {
                                token,
                                retry_at: tokio::time::Instant::now() + RETRY_DELAY,
                            });
                            if unexpected {
                                state.restart_at = Some(tokio::time::Instant::now() + RETRY_DELAY);
                                warn!("keyboard capture session ended unexpectedly, delaying re-arm");
                            }
                            state.current = None;
                            reconcile = true;
                        }
                    }
                }
            }
            result = spec.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            result = receiver_requests.changed() => match result {
                Ok(()) => reconcile = true,
                Err(_) => return,
            },
            open = wait_for_registry_change(
                &mut registry_changes,
                state.pending_restore.is_some(),
            ) => {
                if !open {
                    return;
                }
                state.expedite_pending_restore();
                reconcile = true;
            }
            () = wait_for_deadline(deadline) => {
                reconcile = true;
            }
        }
    }
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
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

fn spawn_session(
    target: KeyboardTarget,
    dispatch: KeyboardDispatchPlan,
    receiver_lease: SessionReceiverLease,
    channels: &KeyboardSessionChannels,
) -> RunningKeyboardSession {
    let (stop_tx, stop_rx) = oneshot::channel();
    let slot = Arc::clone(&channels.capture);
    let session_registry = channels.registry.clone();
    let id = HidppSessionId::new(&dispatch.config_key);
    let (sink, mut session_rx) = mpsc::unbounded_channel();
    let forward_events = channels.events.clone();
    let forward_id = id.clone();
    let forward = tokio::spawn(async move {
        while let Some(input) = session_rx.recv().await {
            let _ = forward_events.send(KeyboardSessionEvent::Input(KeyboardInput {
                session: forward_id.clone(),
                input,
            }));
        }
    });
    let session_events = channels.events.clone();
    let done_id = id.clone();
    let route = target.route.clone();
    let wanted = target.wanted.clone();
    tokio::spawn(async move {
        let _receiver_lease = receiver_lease;
        let pending_restore = match run_keyboard_capture_session_with_registry(
            route,
            wanted,
            sink,
            stop_rx,
            slot,
            &session_registry,
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
        };
        // The device layer drops its listener only after restoration. Draining
        // this forwarder before Done preserves every input accepted while
        // diversion was still active ahead of the ownership boundary.
        let _ = forward.await;
        let _ = session_events.send(KeyboardSessionEvent::Done(KeyboardDone {
            session: done_id,
            pending_restore,
        }));
    });
    CaptureSession::active(id, target, dispatch, stop_tx)
}

#[cfg(test)]
mod tests;
