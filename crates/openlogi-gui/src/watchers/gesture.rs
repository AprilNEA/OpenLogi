//! Background HID++ control-capture watcher for the active device.
//!
//! Runs [`openlogi_hid::run_capture_session`] on a dedicated thread for whichever
//! device the DPI / SmartShift path currently targets
//! ([`DpiCycleState::target`]), restarts it when the carousel selection — or the
//! thumb-wheel binding — changes, and dispatches each captured input:
//!
//! - a gesture swipe through the gesture binding map,
//! - a DPI/ModeShift or thumb-wheel-tap press through the button binding map,
//! - thumb-wheel rotation re-synthesised as horizontal scroll,
//!
//! all via the common action path ([`crate::hook_runtime::dispatch_action`]).
//!
//! Unlike the CGEventTap hook, this needs no macOS Accessibility permission —
//! the events arrive over HID++, and the bound action is synthesised the same
//! way regardless.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};
use openlogi_hid::{CaptureChannel, CapturedInput, DeviceRoute, run_capture_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::hook_runtime::{self, BindingMap};
use crate::state::DpiCycleState;

/// Shared gesture-direction binding map, mirrored from `AppState` (keyed by
/// direction). The watcher reads it to map a captured swipe to a bound action.
pub type GestureBindings = Arc<RwLock<BTreeMap<GestureDirection, Action>>>;

/// How often to re-read the active device target + thumb-wheel binding so a
/// carousel switch or a binding edit re-points / re-arms capture.
const TARGET_POLL: Duration = Duration::from_secs(1);

/// Spawn the capture-manager thread. It owns a current-thread tokio runtime that
/// keeps one capture session pointed at the active device and dispatches each
/// captured input.
pub fn spawn(
    button_bindings: BindingMap,
    gesture_bindings: GestureBindings,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture_channel: CaptureChannel,
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
            button_bindings,
            gesture_bindings,
            dpi_cycle,
            capture_channel,
        ));
    });
}

/// Whether the thumb-wheel click is bound to a non-default action — the only
/// case where we divert the wheel (which suppresses native scroll) to capture
/// its tap, re-synthesising scroll from the rotation events.
fn thumbwheel_armed(button_bindings: &BindingMap) -> bool {
    button_bindings.read().ok().is_some_and(|guard| {
        guard
            .get(&ButtonId::Thumbwheel)
            .is_some_and(|action| *action != default_binding(ButtonId::Thumbwheel))
    })
}

/// Keep one capture session alive for the active device, restarting it when the
/// device or the thumb-wheel arming changes, and dispatch incoming inputs. Runs
/// for the lifetime of the process.
async fn manage(
    button_bindings: BindingMap,
    gesture_bindings: GestureBindings,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture_channel: CaptureChannel,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<CapturedInput>();
    let mut current: Option<(DeviceRoute, bool)> = None;
    let mut stop: Option<oneshot::Sender<()>> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);

    loop {
        tokio::select! {
            Some(input) = rx.recv() => {
                dispatch(input, &button_bindings, &gesture_bindings, &dpi_cycle, &capture_channel);
            }
            _ = ticker.tick() => {
                let target = dpi_cycle.read().ok().and_then(|guard| guard.target.clone());
                let want = target.map(|t| (t, thumbwheel_armed(&button_bindings)));
                if want == current {
                    continue;
                }
                // Target or thumb-wheel arming changed (or first tick): stop the
                // old session and start one for the new state. Sending on the
                // oneshot lets the old session restore the diverted controls.
                if let Some(stop) = stop.take() {
                    let _ = stop.send(());
                }
                current.clone_from(&want);
                if let Some((route, capture_thumbwheel)) = want {
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let sink = tx.clone();
                    let slot = Arc::clone(&capture_channel);
                    tokio::spawn(async move {
                        if let Err(e) =
                            run_capture_session(route, capture_thumbwheel, sink, stop_rx, slot).await
                        {
                            debug!(error = %e, "capture session ended");
                        }
                    });
                    stop = Some(stop_tx);
                }
            }
        }
    }
}

/// Route one captured input to its bound action (or re-synthesised scroll).
fn dispatch(
    input: CapturedInput,
    button_bindings: &BindingMap,
    gesture_bindings: &GestureBindings,
    dpi_cycle: &Arc<RwLock<DpiCycleState>>,
    capture: &CaptureChannel,
) {
    match input {
        CapturedInput::Gesture(direction) => {
            let action = gesture_bindings
                .read()
                .ok()
                .and_then(|guard| guard.get(&direction).cloned());
            if let Some(action) = action {
                debug!(?direction, action = %action.label(), "gesture → action");
                hook_runtime::dispatch_action(&action, dpi_cycle, capture);
            } else {
                debug!(?direction, "gesture with no binding — ignored");
            }
        }
        CapturedInput::ButtonPressed(button) => {
            let action = button_bindings
                .read()
                .ok()
                .and_then(|guard| guard.get(&button).cloned());
            if let Some(action) = action {
                debug!(?button, action = %action.label(), "HID++ button → action");
                hook_runtime::dispatch_action(&action, dpi_cycle, capture);
            } else {
                debug!(?button, "HID++ button with no binding — ignored");
            }
        }
        CapturedInput::Scroll(rotation) => {
            // Re-inject native horizontal scroll the diverted thumb wheel no
            // longer produces. Sign/magnitude may need per-device tuning.
            openlogi_core::binding::post_horizontal_scroll(i32::from(rotation));
        }
    }
}
