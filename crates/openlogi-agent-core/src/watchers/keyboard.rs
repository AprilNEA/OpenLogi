//! Background HID++ key-capture watcher for a bound keyboard.
//!
//! Runs [`openlogi_hid::run_keyboard_capture_session`] on a dedicated thread
//! for the keyboard the orchestrator publishes in [`SharedKeyboardSpec`],
//! restarts it when the keyboard (or the set of bound keys) changes, and
//! dispatches each captured key press through the common action path
//! ([`crate::hook_runtime::dispatch_action`]).
//!
//! The mouse capture watcher ([`super::gesture`]) and this one hold *shared*
//! receiver leases, so both run concurrently; pairing still waits for (and
//! excludes) both. Like the gesture watcher, this needs no macOS Accessibility
//! permission — the key events arrive over HID++.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use openlogi_core::binding::{Action, ButtonId};
use openlogi_hid::{CaptureChannel, CapturedInput, DeviceRoute, run_keyboard_capture_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::DpiCycleState;
use crate::hook_runtime;
use crate::receiver_access::ReceiverAccess;
use crate::watchers::gesture::should_rearm;

/// Everything the watcher needs to capture one keyboard: where it is, which
/// `0x1b04` controls to divert (only keys carrying a real binding), and the
/// per-key action map presses dispatch through. Rebuilt by the orchestrator on
/// config / inventory / foreground-app changes.
#[derive(Clone)]
pub struct KeyboardSpec {
    /// HID++ route of the keyboard.
    pub route: DeviceRoute,
    /// `0x1b04` control ID → button, for exactly the bound keys.
    pub wanted: BTreeMap<u16, ButtonId>,
    /// Effective per-key single-action map (per-app overlay applied).
    pub bindings: BTreeMap<ButtonId, Action>,
}

/// Shared keyboard-capture spec, `None` when no online keyboard has bound
/// keys. Written by the orchestrator, read by the watcher.
pub type SharedKeyboardSpec = Arc<RwLock<Option<KeyboardSpec>>>;

/// How often to re-read the spec so a config edit, per-app overlay change, or
/// keyboard reconnect re-points the capture session.
const TARGET_POLL: Duration = Duration::from_secs(1);

/// Spawn the keyboard-capture manager thread. It owns a current-thread tokio
/// runtime that keeps one capture session pointed at the bound keyboard and
/// dispatches each captured key press.
pub fn spawn(
    spec: SharedKeyboardSpec,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    mouse_capture: CaptureChannel,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
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
            dpi_cycle,
            mouse_capture,
            keyboard_channel,
            receiver_access,
        ));
    });
}

/// Keep one keyboard capture session alive for the published spec, restarting
/// it when the keyboard or its bound-key set changes, and dispatch incoming
/// presses. Runs for the lifetime of the process.
async fn manage(
    spec: SharedKeyboardSpec,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    mouse_capture: CaptureChannel,
    keyboard_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<CapturedInput>();
    let mut current: Option<(DeviceRoute, BTreeMap<u16, ButtonId>)> = None;
    let mut stop: Option<oneshot::Sender<()>> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);
    // Sessions report completion tagged with their start epoch, so an
    // unexpected exit of the *current* session re-arms while stale completions
    // are ignored — same pacing/starvation reasoning as the gesture watcher.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<u64>();
    let mut epoch: u64 = 0;

    loop {
        tokio::select! {
            Some(input) = rx.recv() => {
                // The keyboard session only emits ButtonPressed; other inputs
                // (gesture/scroll) never originate here.
                let CapturedInput::ButtonPressed(button) = input else {
                    continue;
                };
                let action = spec
                    .read()
                    .ok()
                    .and_then(|guard| {
                        guard.as_ref().and_then(|s| s.bindings.get(&button).cloned())
                    });
                if let Some(action) = action {
                    info!(button = %button, action = %action.label(), "keyboard key → executing bound action");
                    hook_runtime::dispatch_action(&action, &dpi_cycle, &mouse_capture);
                } else {
                    debug!(?button, "keyboard key with no binding — ignored");
                }
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release the capture
                // session so run_pairing can own the receiver's HID node.
                let want = if receiver_access.pairing_requested() {
                    None
                } else {
                    spec.read()
                        .ok()
                        .and_then(|guard| guard.clone())
                        .map(|s| (s.route, s.wanted))
                };
                if want == current {
                    continue;
                }
                // Spec changed (or first tick): stop the old session and start
                // one for the new state. Sending on the oneshot lets the old
                // session restore the diverted controls.
                if let Some(stop) = stop.take() {
                    let _ = stop.send(());
                }
                if current.is_some() {
                    current = None;
                    continue;
                }
                if let Some((route, wanted)) = want {
                    let Some(receiver_lease) = receiver_access.try_acquire_for_capture() else {
                        current = None;
                        continue;
                    };
                    current = Some((route.clone(), wanted.clone()));
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let sink = tx.clone();
                    let slot = Arc::clone(&keyboard_channel);
                    epoch = epoch.wrapping_add(1);
                    let session_epoch = epoch;
                    let done = done_tx.clone();
                    tokio::spawn(async move {
                        let _receiver_lease = receiver_lease;
                        if let Err(e) =
                            run_keyboard_capture_session(route, wanted, sink, stop_rx, slot).await
                        {
                            debug!(error = %e, "keyboard capture session ended");
                        }
                        let _ = done.send(session_epoch);
                    });
                    stop = Some(stop_tx);
                } else {
                    current = None;
                }
            }
            Some(done_epoch) = done_rx.recv() => {
                // A capture session ended on its own; re-arm only the live one
                // (see gesture watcher for the epoch/pacing rationale).
                if should_rearm(done_epoch, epoch, current.is_some()) {
                    warn!("keyboard capture session ended unexpectedly, re-arming");
                    current = None;
                    stop = None;
                }
            }
        }
    }
}
