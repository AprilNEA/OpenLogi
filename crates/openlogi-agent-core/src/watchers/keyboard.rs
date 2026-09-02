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
    CaptureChannel, CapturedInput, ChannelRegistry, DeviceRoute,
    run_keyboard_capture_session_with_registry,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::gesture::DoneAction;
use crate::receiver_access::ReceiverAccess;
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// Everything the watcher needs to capture one keyboard: where it is, which
/// `0x1b04` controls to divert (only keys carrying a real binding), and the
/// per-key action map presses dispatch through. Rebuilt by the orchestrator on
/// config / inventory / foreground-app changes.
#[derive(Clone)]
pub struct KeyboardSpec {
    /// Stable config key used to scope lifecycle cancellation and hardware
    /// actions to this keyboard.
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
#[derive(Clone, PartialEq)]
struct KeyboardTarget {
    config_key: String,
    route: DeviceRoute,
    wanted: BTreeMap<u16, ButtonId>,
}

impl KeyboardTarget {
    fn for_spec(spec: KeyboardSpec) -> Self {
        Self {
            config_key: spec.config_key,
            route: spec.route,
            wanted: spec.wanted,
        }
    }

    fn matches(&self, spec: &KeyboardSpec) -> bool {
        self.config_key == spec.config_key && self.route == spec.route && self.wanted == spec.wanted
    }
}

struct RunningKeyboardSession {
    id: HidppSessionId,
    target: KeyboardTarget,
    /// Present while the session runs; taken to request a stop. `None` means
    /// the session is draining — deliberately stopped, but its task (and the
    /// control-restore writes in its teardown) may still be in flight.
    stop: Option<oneshot::Sender<()>>,
}

/// Decide the [`DoneAction`] for a completion report, given the session the
/// manager currently tracks. The gesture manager's rule, applied to the
/// single keyboard slot: only the current session's report settles anything;
/// one whose stop sender is gone was stopped deliberately and merely frees
/// the slot, while one still holding it exited on its own and warrants a
/// warning alongside the re-arm.
fn on_done(done_session: &HidppSessionId, live: Option<&RunningKeyboardSession>) -> DoneAction {
    match live {
        Some(session) if session.id == *done_session => DoneAction::Remove {
            unexpected: session.stop.is_some(),
        },
        _ => DoneAction::Ignore,
    }
}

/// Whether an input belongs to the current, still-live session. A draining
/// session has already had its presses cancelled, so even its correctly
/// tagged queued events must not enter the replacement lifecycle.
fn accepts_input(input_session: &HidppSessionId, live: Option<&RunningKeyboardSession>) -> bool {
    live.is_some_and(|session| session.id == *input_session && session.stop.is_some())
}

struct KeyboardInput {
    session: HidppSessionId,
    input: CapturedInput,
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
    input: &CapturedInput,
    spec: &KeyboardSpec,
    dispatcher: &ActionDispatcher,
) {
    match *input {
        CapturedInput::ButtonDown(button) => {
            let binding = spec.bindings.get(&button);
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
            dispatcher.dispatch_hidpp_button_pulse(session, button, spec.bindings.get(&button));
        }
        CapturedInput::Gesture(..) | CapturedInput::Scroll { .. } => {}
    }
}

/// Snapshot the keyboard session target unless pairing currently owns capture.
fn wanted_session(
    receiver_access: &ReceiverAccess,
    spec: &SharedKeyboardSpec,
) -> Option<KeyboardTarget> {
    if receiver_access.exclusive_requested() {
        return None;
    }
    spec.read()
        .ok()
        .and_then(|guard| guard.clone())
        .map(KeyboardTarget::for_spec)
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
    let (tx, mut rx) = mpsc::unbounded_channel::<KeyboardInput>();
    let mut current: Option<RunningKeyboardSession> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);
    // Sessions report completion tagged with their start epoch, so an
    // unexpected exit of the *current* session re-arms while stale completions
    // are ignored — same pacing/starvation reasoning as the gesture watcher.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<HidppSessionId>();

    loop {
        tokio::select! {
            Some(input) = rx.recv() => {
                let live_spec = spec.read().ok().and_then(|guard| guard.clone());
                let deliverable = accepts_input(&input.session, current.as_ref())
                    && !receiver_access.exclusive_requested()
                    && current
                        .as_ref()
                        .zip(live_spec.as_ref())
                        .is_some_and(|(running, live)| running.target.matches(live));
                if !deliverable {
                    dispatcher.cancel_hidpp_session(&input.session);
                    debug!(epoch = input.session.epoch(), "input from a stale keyboard session — ignored");
                    continue;
                }
                let Some(live_spec) = live_spec else {
                    continue;
                };
                dispatch_input(&input.session, &input.input, &live_spec, &dispatcher);
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release the capture
                // session so run_pairing can own the receiver's HID node.
                let want = wanted_session(&receiver_access, &spec);
                if let Some(running) = current.as_mut() {
                    // Stop a session that no longer matches the spec; sending
                    // on the oneshot lets it restore the diverted controls.
                    // The entry stays tracked — stop sender taken — until its
                    // task reports completion below, and a tracked keyboard
                    // is never re-armed: arming the replacement while the old
                    // task may still be mid-restore could interleave its
                    // divert writes with the restore writes on the same
                    // device, leaving a control un-diverted while the new
                    // session believes it owns it (the gesture manager
                    // documents the same hazard).
                    let keep = want.as_ref() == Some(&running.target);
                    if !keep && let Some(stop) = running.stop.take() {
                        dispatcher.cancel_hidpp_session(&running.id);
                        let _ = stop.send(());
                    }
                    continue;
                }
                if let Some(target) = want {
                    let Some(receiver_lease) = receiver_access.try_acquire_for_session() else {
                        continue;
                    };
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let slot = Arc::clone(&keyboard_channel);
                    let session_registry = registry.clone();
                    let id = HidppSessionId::new(&target.config_key);
                    let (sink, mut session_rx) = mpsc::unbounded_channel();
                    let forward = tx.clone();
                    let forward_id = id.clone();
                    tokio::spawn(async move {
                        while let Some(input) = session_rx.recv().await {
                            let _ = forward.send(KeyboardInput {
                                session: forward_id.clone(),
                                input,
                            });
                        }
                    });
                    let done = done_tx.clone();
                    let done_id = id.clone();
                    let route = target.route.clone();
                    let wanted = target.wanted.clone();
                    tokio::spawn(async move {
                        let _receiver_lease = receiver_lease;
                        if let Err(e) = run_keyboard_capture_session_with_registry(
                            route,
                            wanted,
                            sink,
                            stop_rx,
                            slot,
                            &session_registry,
                        )
                        .await
                        {
                            debug!(error = %e, "keyboard capture session ended");
                        }
                        let _ = done.send(done_id);
                    });
                    current = Some(RunningKeyboardSession {
                        id,
                        target,
                        stop: Some(stop_tx),
                    });
                }
            }
            Some(done_session) = done_rx.recv() => {
                // The session's task has fully exited — restore writes
                // included — so clearing the slot lets the next tick arm a
                // successor, paced by TARGET_POLL; a stale epoch belongs to a
                // session already superseded (see `on_done`).
                if let DoneAction::Remove { unexpected } = on_done(&done_session, current.as_ref()) {
                    dispatcher.cancel_hidpp_session(&done_session);
                    if unexpected {
                        warn!("keyboard capture session ended unexpectedly, re-arming");
                    }
                    current = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> KeyboardTarget {
        KeyboardTarget {
            config_key: "keyboard-a".to_string(),
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

    fn draining_session(epoch: u64) -> RunningKeyboardSession {
        RunningKeyboardSession {
            id: session_id(epoch),
            target: target(),
            stop: None,
        }
    }

    fn live_session(epoch: u64) -> RunningKeyboardSession {
        let (stop, _rx) = oneshot::channel();
        RunningKeyboardSession {
            stop: Some(stop),
            ..draining_session(epoch)
        }
    }

    #[test]
    fn rearms_when_the_current_session_dies() {
        assert_eq!(
            on_done(&session_id(7), Some(&live_session(7))),
            DoneAction::Remove { unexpected: true }
        );
    }

    #[test]
    fn settles_a_draining_session_quietly() {
        assert_eq!(
            on_done(&session_id(7), Some(&draining_session(7))),
            DoneAction::Remove { unexpected: false }
        );
    }

    #[test]
    fn ignores_stale_and_untracked_completions() {
        assert_eq!(
            on_done(&session_id(6), Some(&live_session(7))),
            DoneAction::Ignore
        );
        assert_eq!(on_done(&session_id(7), None), DoneAction::Ignore);
    }

    #[test]
    fn accepts_inputs_only_from_the_current_live_session() {
        assert!(accepts_input(&session_id(7), Some(&live_session(7))));
        assert!(
            !accepts_input(&session_id(6), Some(&live_session(7))),
            "a superseded session's queued input is stale"
        );
        assert!(
            !accepts_input(&session_id(7), Some(&draining_session(7))),
            "a draining session's queued input must not enter the replacement lifecycle"
        );
        assert!(!accepts_input(&session_id(7), None));
    }
}
