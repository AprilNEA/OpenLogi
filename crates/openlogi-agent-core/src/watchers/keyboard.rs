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
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use openlogi_core::binding::{Binding, ButtonId};
use openlogi_hid::{
    CaptureChannel, CaptureSessionOutcome, CapturedInput, ChannelRegistry, DeviceRoute,
    PendingCaptureRestore, run_keyboard_capture_session_with_registry,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::capture_session::{CaptureSession, CompletionAction, ReconcileAction};
use crate::receiver_access::{ReceiverAccess, SessionReceiverLease};
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Everything the watcher needs to capture one keyboard: where it is, which
/// `0x1b04` controls to divert (only keys carrying a real binding), and the
/// per-key action map presses dispatch through. Rebuilt by the orchestrator on
/// config / inventory / foreground-app changes.
#[derive(Clone)]
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

/// Shared keyboard-capture spec, `None` when no online keyboard has bound
/// keys. Written by the orchestrator, read by the watcher.
pub type SharedKeyboardSpec = Arc<RwLock<Option<KeyboardSpec>>>;

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

/// How often to re-read the spec so a config edit, per-app overlay change, or
/// keyboard reconnect re-points the capture session.
const TARGET_POLL: Duration = Duration::from_secs(1);

/// Spawn the keyboard-capture manager thread. It owns a current-thread tokio
/// runtime that keeps one capture session pointed at the bound keyboard and
/// dispatches each captured key press.
pub fn spawn(
    spec: SharedKeyboardSpec,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    dispatcher: ActionDispatcher,
) {
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
    receiver_access: &ReceiverAccess,
    spec: &SharedKeyboardSpec,
) -> Option<(KeyboardTarget, KeyboardDispatchPlan)> {
    if receiver_access.exclusive_requested() {
        return None;
    }
    spec.read()
        .ok()
        .and_then(|guard| guard.clone())
        .map(|spec| {
            (
                KeyboardTarget::for_spec(&spec),
                KeyboardDispatchPlan {
                    config_key: spec.config_key,
                    bindings: spec.bindings,
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

/// Keep one keyboard capture session alive for the published spec, restarting
/// it when the keyboard or its bound-key set changes, and dispatch incoming
/// presses. Runs for the lifetime of the process.
async fn manage(
    spec: SharedKeyboardSpec,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    registry: ChannelRegistry,
    dispatcher: ActionDispatcher,
) {
    let (events, mut event_rx) = mpsc::unbounded_channel::<KeyboardSessionEvent>();
    let mut current: Option<RunningKeyboardSession> = None;
    let mut pending_restore: Option<PendingCaptureRestore> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    KeyboardSessionEvent::Input(input) => {
                        let want = wanted_session(&receiver_access, &spec);
                        if let Some(running) = current.as_mut() {
                            reconcile_session(running, want.as_ref(), &dispatcher);
                        }

                        let Some(running) = current
                            .as_ref()
                            .filter(|running| running.owns(&input.session))
                        else {
                            dispatcher.cancel_hidpp_session(&input.session);
                            debug!(epoch = input.session.epoch(), "input from a stale keyboard session — ignored");
                            continue;
                        };
                        dispatch_input(
                            running.id(),
                            input.input,
                            running.dispatch(),
                            &dispatcher,
                        );
                    }
                    KeyboardSessionEvent::Done(done) => {
                        // Input and Done share this queue, and the forwarding
                        // task is drained before Done is sent. A tracked
                        // draining session therefore remains the sole input
                        // owner until firmware restoration is complete.
                        if let Some((CompletionAction::Remove { unexpected }, dispatch_session)) =
                            current.as_ref().map(|running| {
                                (running.completion(&done.session), running.id().clone())
                            })
                        {
                            dispatcher.cancel_hidpp_session(&dispatch_session);
                            pending_restore = done.pending_restore;
                            if unexpected {
                                warn!("keyboard capture session ended unexpectedly, re-arming");
                            }
                            current = None;
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release the capture
                // session so run_pairing can own the receiver's HID node.
                let want = wanted_session(&receiver_access, &spec);
                if let Some(running) = current.as_mut() {
                    reconcile_session(running, want.as_ref(), &dispatcher);
                    continue;
                }
                // Restoration remains mandatory when the spec disappears
                // (for example, after disabling the keyboard). Retry the
                // consuming token before considering any successor. A
                // successful restore hands the same lease into that successor
                // so pairing cannot enter between restore and rearm.
                let mut handoff_lease = None;
                if let Some(pending) = pending_restore.take() {
                    let Some(lease) = receiver_access.try_acquire_for_session() else {
                        pending_restore = Some(pending);
                        continue;
                    };
                    handoff_lease = Some(lease);
                    if let CaptureSessionOutcome::RestorePending(pending) =
                        pending.retry(&registry).await
                    {
                        pending_restore = Some(pending);
                        continue;
                    }
                }
                if let Some((target, dispatch)) = want {
                    let receiver_lease = if let Some(lease) = handoff_lease {
                        lease
                    } else {
                        let Some(lease) = receiver_access.try_acquire_for_session() else {
                            continue;
                        };
                        lease
                    };
                    current = Some(spawn_session(
                        target,
                        dispatch,
                        receiver_lease,
                        &keyboard_channel,
                        &registry,
                        &events,
                    ));
                }
            }
        }
    }
}

fn spawn_session(
    target: KeyboardTarget,
    dispatch: KeyboardDispatchPlan,
    receiver_lease: SessionReceiverLease,
    keyboard_channel: &CaptureChannel,
    registry: &ChannelRegistry,
    events: &mpsc::UnboundedSender<KeyboardSessionEvent>,
) -> RunningKeyboardSession {
    let (stop_tx, stop_rx) = oneshot::channel();
    let slot = Arc::clone(keyboard_channel);
    let session_registry = registry.clone();
    let id = HidppSessionId::new(&dispatch.config_key);
    let (sink, mut session_rx) = mpsc::unbounded_channel();
    let forward_events = events.clone();
    let forward_id = id.clone();
    let forward = tokio::spawn(async move {
        while let Some(input) = session_rx.recv().await {
            let _ = forward_events.send(KeyboardSessionEvent::Input(KeyboardInput {
                session: forward_id.clone(),
                input,
            }));
        }
    });
    let session_events = events.clone();
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
mod tests {
    use super::*;
    use openlogi_core::binding::Action;

    fn target() -> KeyboardTarget {
        KeyboardTarget {
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc548,
            },
            wanted: BTreeMap::new(),
        }
    }

    fn session_id(epoch: u64) -> HidppSessionId {
        HidppSessionId::with_epoch("keyboard-a", epoch)
    }

    fn dispatch(action: Action) -> KeyboardDispatchPlan {
        KeyboardDispatchPlan {
            config_key: "keyboard-a".to_owned(),
            bindings: BTreeMap::from([(ButtonId::KeySearch, Binding::Single(action))]),
        }
    }

    fn live_session(epoch: u64) -> RunningKeyboardSession {
        let (stop, _rx) = oneshot::channel();
        CaptureSession::active(
            session_id(epoch),
            target(),
            dispatch(Action::MissionControl),
            stop,
        )
    }

    fn draining_session(epoch: u64) -> RunningKeyboardSession {
        let mut session = live_session(epoch);
        assert_eq!(session.reconcile(None), ReconcileAction::Retiring);
        session
    }

    #[test]
    fn accepts_inputs_from_the_current_session_until_teardown_finishes() {
        assert!(live_session(7).owns(&session_id(7)));
        assert!(
            !live_session(7).owns(&session_id(6)),
            "a superseded session's queued input is stale"
        );
        assert!(
            draining_session(7).owns(&session_id(7)),
            "the draining keyboard remains the sole owner until restore and ordered Done"
        );
    }

    #[test]
    fn binding_changes_refresh_without_rearming_hardware() {
        let mut session = live_session(7);
        let current_target = session.target().clone();
        let new_dispatch = dispatch(Action::ShowDesktop);

        assert_eq!(
            session.reconcile(Some((&current_target, &new_dispatch))),
            ReconcileAction::DispatchChanged
        );
        assert!(session.is_active());
        assert_eq!(session.dispatch(), &new_dispatch);
    }

    #[test]
    fn target_changes_freeze_dispatch_until_teardown_finishes() {
        let mut session = live_session(7);
        let old_dispatch = session.dispatch().clone();
        let mut replacement = target();
        replacement.wanted.insert(0x00d4, ButtonId::KeySearch);
        let new_dispatch = dispatch(Action::ShowDesktop);

        assert!(
            replacement != *session.target(),
            "the test must require different firmware capture"
        );
        assert_eq!(
            session.reconcile(Some((&replacement, &new_dispatch))),
            ReconcileAction::Retiring
        );
        assert!(!session.is_active());
        assert_eq!(session.dispatch(), &old_dispatch);
    }
}
