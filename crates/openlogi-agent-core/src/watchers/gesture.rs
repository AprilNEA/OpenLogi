//! Background HID++ control-capture watcher for the active device.
//!
//! Runs [`openlogi_hid::run_capture_session`] on a dedicated thread for whichever
//! device the DPI / SmartShift path currently targets
//! ([`DpiCycleState::target`]), restarts it when the carousel selection — or the
//! thumb-wheel arming — changes, and dispatches each captured input:
//!
//! - a gesture swipe through the gesture binding map,
//! - a DPI/ModeShift or thumb-wheel-tap press through the button binding map,
//! - thumb-wheel rotation through the [`ButtonId::ThumbwheelScrollUp`] /
//!   [`ButtonId::ThumbwheelScrollDown`] bindings — either re-synthesised as
//!   continuous, sensitivity-scaled horizontal scroll or accumulated into a
//!   custom action,
//!
//! all via the common action path ([`crate::hook_runtime::dispatch_action`]).
//!
//! Unlike the CGEventTap hook, this needs no macOS Accessibility permission —
//! the events arrive over HID++, and the bound action is synthesised the same
//! way regardless.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::DEFAULT_THUMBWHEEL_SENSITIVITY;
use openlogi_hid::{CaptureChannel, CapturedInput, DeviceRoute, run_capture_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::DpiCycleState;
use crate::hook_runtime::{self, SharedHookMaps};

/// Shared gesture-direction binding map, mirrored from `AppState` (keyed by
/// direction). The watcher reads it to map a captured swipe to a bound action.
pub type GestureBindings = Arc<RwLock<BTreeMap<GestureDirection, Action>>>;

/// Shared thumb-wheel sensitivity, mirrored from `AppState`. Read on every wheel
/// event; written only by `AppState::set_thumbwheel_sensitivity`.
pub type ThumbwheelSensitivity = Arc<AtomicI32>;

/// How often to re-read the active device target + thumb-wheel arming so a
/// carousel switch or a binding/sensitivity edit re-points / re-arms capture.
/// It also paces the respawn of a session that ended on its own (see `manage`).
const TARGET_POLL: Duration = Duration::from_secs(1);

/// Idle gap after which a partly-accumulated *custom* wheel action is forgotten,
/// so slow intermittent nudges don't eventually cross the threshold.
const ACTION_DECAY: Duration = Duration::from_millis(300);

/// Minimum gap between two fires of the same custom wheel action, so one
/// deliberate flick triggers once instead of repeating across a fast spin.
const ACTION_COOLDOWN: Duration = Duration::from_millis(200);

/// Speed multiplier for the wheel's continuous horizontal scroll. The default
/// sensitivity is 1×; the scale is linear around it.
#[allow(
    clippy::cast_precision_loss,
    reason = "sensitivity is a small 1..=100 integer — exact in f32"
)]
fn scroll_multiplier(sensitivity: i32) -> f32 {
    sensitivity as f32 / DEFAULT_THUMBWHEEL_SENSITIVITY as f32
}

/// Rotation increments required to fire a custom (non-scroll) wheel action.
/// Higher sensitivity → fewer increments; always at least one.
fn action_threshold(sensitivity: i32) -> i32 {
    (2 * DEFAULT_THUMBWHEEL_SENSITIVITY - sensitivity).max(1)
}

/// Spawn the capture-manager thread. It owns a current-thread tokio runtime that
/// keeps one capture session pointed at the active device and dispatches each
/// captured input.
pub fn spawn(
    hook_maps: SharedHookMaps,
    gesture_bindings: GestureBindings,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture_channel: CaptureChannel,
    thumbwheel_sensitivity: ThumbwheelSensitivity,
    pairing_active: Arc<AtomicBool>,
    capture_idle: Arc<AtomicBool>,
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
            hook_maps,
            gesture_bindings,
            dpi_cycle,
            capture_channel,
            thumbwheel_sensitivity,
            pairing_active,
            capture_idle,
        ));
    });
}

/// Whether the thumb wheel must be diverted over HID++ (which suppresses native
/// scroll) so we can re-synthesise its scroll or capture its tap.
///
/// We divert when the sensitivity leaves its default (so we can scale scroll
/// ourselves) or when the click or either rotation direction is rebound away
/// from its default; otherwise the OS scrolls the wheel natively.
fn thumbwheel_armed(hook_maps: &SharedHookMaps, sensitivity: i32) -> bool {
    if sensitivity != DEFAULT_THUMBWHEEL_SENSITIVITY {
        return true;
    }
    hook_maps.read().ok().is_some_and(|maps| {
        [
            ButtonId::Thumbwheel,
            ButtonId::ThumbwheelScrollUp,
            ButtonId::ThumbwheelScrollDown,
        ]
        .iter()
        .any(|&button| {
            maps.bindings
                .get(&button)
                .is_some_and(|action| *action != default_binding(button))
        })
    })
}

/// Whether a finished capture session should make the manager re-arm.
///
/// `done_epoch` identifies the session that signalled completion; `live_epoch`
/// is the session the manager currently believes is running; `has_target` is
/// whether a device is currently targeted. A session ending only warrants a
/// respawn when it is the *current* one (not a stale session already superseded
/// by a deliberate restart, whose epoch no longer matches) and a target is still
/// set (not a deliberate stop-to-idle, e.g. while pairing owns the receiver).
fn should_rearm(done_epoch: u64, live_epoch: u64, has_target: bool) -> bool {
    done_epoch == live_epoch && has_target
}

/// Keep one capture session alive for the active device, restarting it when the
/// device or the thumb-wheel arming changes, and dispatch incoming inputs. Runs
/// for the lifetime of the process.
async fn manage(
    hook_maps: SharedHookMaps,
    gesture_bindings: GestureBindings,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture_channel: CaptureChannel,
    thumbwheel_sensitivity: ThumbwheelSensitivity,
    pairing_active: Arc<AtomicBool>,
    capture_idle: Arc<AtomicBool>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<CapturedInput>();
    // (route, capture_thumbwheel, divert_gesture_button)
    let mut current: Option<(DeviceRoute, bool, bool)> = None;
    let mut stop: Option<oneshot::Sender<()>> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);
    let mut accumulators = WheelAccumulators::default();
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed: the tick below only restarts on a *changed* target, so a session
    // that dies with the target unchanged leaves the gesture button and thumb
    // wheel dead until the next carousel switch, config reload, or agent restart.
    // Each session reports its completion here, tagged with the epoch it started
    // under, so a dead *current* session can be re-armed while stale completions
    // (from an already-superseded session) are ignored.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<u64>();
    let mut epoch: u64 = 0;

    loop {
        tokio::select! {
            Some(input) = rx.recv() => {
                dispatch(
                    input,
                    &mut accumulators,
                    &hook_maps,
                    &gesture_bindings,
                    &dpi_cycle,
                    &capture_channel,
                    &thumbwheel_sensitivity,
                );
            }
            _ = ticker.tick() => {
                // While pairing, release the capture session so run_pairing can
                // own the receiver's HID node (one process can't read it through
                // two channels). The pairing manager waits on `capture_idle`.
                let want = if pairing_active.load(Ordering::Relaxed) {
                    None
                } else {
                    let target = dpi_cycle.read().ok().and_then(|guard| guard.target.clone());
                    let sensitivity = thumbwheel_sensitivity.load(Ordering::Relaxed);
                    // Divert the thumb pad only while it owns the gesture role. The
                    // shared gesture map is non-empty exactly then (gesture_bindings_for
                    // gates on the owner), so it doubles as that signal — no need to
                    // thread the full config in. Re-evaluated each tick, so a
                    // ReloadConfig owner change restarts the session accordingly.
                    let divert_gesture = gesture_bindings.read().is_ok_and(|g| !g.is_empty());
                    target.map(|t| {
                        (
                            t,
                            thumbwheel_armed(&hook_maps, sensitivity),
                            divert_gesture,
                        )
                    })
                };
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
                capture_idle.store(want.is_none(), Ordering::Relaxed);
                if let Some((route, capture_thumbwheel, divert_gesture_button)) = want {
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let sink = tx.clone();
                    let slot = Arc::clone(&capture_channel);
                    epoch = epoch.wrapping_add(1);
                    let session_epoch = epoch;
                    let done = done_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = run_capture_session(
                            route,
                            capture_thumbwheel,
                            divert_gesture_button,
                            sink,
                            stop_rx,
                            slot,
                        )
                        .await
                        {
                            debug!(error = %e, "capture session ended");
                        }
                        // Report completion so the manager can re-arm if this exit
                        // was unexpected rather than a deliberate stop.
                        let _ = done.send(session_epoch);
                    });
                    stop = Some(stop_tx);
                }
            }
            Some(done_epoch) = done_rx.recv() => {
                // A capture session ended on its own. Re-arm only when it is the
                // session we currently believe is live for an active target;
                // clearing `current` lets the next tick start a fresh session.
                // The tick fires at most once per `TARGET_POLL`, which paces the
                // respawn so a permanently failing device can't hot-loop. A stale
                // epoch or a deliberate stop-to-idle is a no-op (see `should_rearm`).
                if should_rearm(done_epoch, epoch, current.is_some()) {
                    warn!("capture session for the active device ended unexpectedly, re-arming");
                    current = None;
                    // Keep the `stop`/`current` invariant: the session already
                    // exited, so its stop receiver is gone and dropping the sender
                    // here is a no-op, but it stops the next tick from signalling a
                    // session that no longer exists.
                    stop = None;
                    // No session owns the receiver now, so mark capture idle: the
                    // tick's `want == current` short-circuit (None == None while
                    // pairing wants None) would otherwise skip this update and a
                    // pairing session waiting on `capture_idle` could stall.
                    capture_idle.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Per-direction wheel accumulators. The thumb wheel's two rotation directions
/// bind to independent actions, so each keeps its own running total — sharing
/// one would let a reversal cancel the other direction's progress.
#[derive(Default)]
struct WheelAccumulators {
    up: WheelDirection,
    down: WheelDirection,
}

/// Running state for one rotation direction.
#[derive(Default)]
struct WheelDirection {
    /// Fractional line accumulator for continuous horizontal scroll.
    scroll: f32,
    /// Integer rotation-increment accumulator for a custom (non-scroll) action.
    action: i32,
    /// When the last rotation event for this direction arrived (decay clock).
    last_event: Option<Instant>,
    /// When this direction last fired its custom action (cooldown clock).
    last_fired: Option<Instant>,
}

/// What advancing a direction's accumulator should produce.
#[derive(Debug, PartialEq)]
enum WheelOutput {
    /// Below threshold / suppressed — emit nothing.
    Idle,
    /// Post this many horizontal scroll lines (signed: + right, − left).
    Scroll(i32),
    /// Fire the direction's bound custom action.
    FireAction,
}

/// Route one captured input to its bound action (or re-synthesised scroll).
fn dispatch(
    input: CapturedInput,
    accumulators: &mut WheelAccumulators,
    hook_maps: &SharedHookMaps,
    gesture_bindings: &GestureBindings,
    dpi_cycle: &Arc<RwLock<DpiCycleState>>,
    capture: &CaptureChannel,
    thumbwheel_sensitivity: &ThumbwheelSensitivity,
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
            let action = hook_maps
                .read()
                .ok()
                .and_then(|maps| maps.bindings.get(&button).cloned());
            if let Some(action) = action {
                debug!(?button, action = %action.label(), "HID++ button → action");
                hook_runtime::dispatch_action(&action, dpi_cycle, capture);
            } else {
                debug!(?button, "HID++ button with no binding — ignored");
            }
        }
        CapturedInput::Scroll(rotation) => {
            // Positive rotation is "up"; each direction has its own binding.
            let up = rotation >= 0;
            let button = if up {
                ButtonId::ThumbwheelScrollUp
            } else {
                ButtonId::ThumbwheelScrollDown
            };
            let action = hook_maps
                .read()
                .ok()
                .and_then(|maps| maps.bindings.get(&button).cloned())
                .unwrap_or_else(|| default_binding(button));
            let sensitivity = thumbwheel_sensitivity.load(Ordering::Relaxed);
            let dir = if up {
                &mut accumulators.up
            } else {
                &mut accumulators.down
            };
            let magnitude = i32::from(rotation).abs();
            match advance(dir, &action, magnitude, sensitivity, Instant::now()) {
                WheelOutput::Idle => {}
                WheelOutput::Scroll(lines) => {
                    openlogi_core::binding::post_horizontal_scroll(lines);
                }
                WheelOutput::FireAction => {
                    debug!(?button, action = %action.label(), "thumb wheel → action");
                    hook_runtime::dispatch_action(&action, dpi_cycle, capture);
                }
            }
        }
    }
}

/// Advance one direction's accumulator by `magnitude` rotation increments and
/// decide what to emit. Pure given `now`, so the decay/cooldown/threshold logic
/// is unit-testable without touching the OS.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "magnitude/sensitivity are small integers and `lines` is a trunc'd \
              whole number — both well within f32/i32 range"
)]
fn advance(
    dir: &mut WheelDirection,
    action: &Action,
    magnitude: i32,
    sensitivity: i32,
    now: Instant,
) -> WheelOutput {
    match action {
        // Suppressed: captured but produces nothing.
        Action::None => WheelOutput::Idle,
        // Continuous, sensitivity-scaled horizontal scroll. Direction comes
        // from the action; magnitude from the accumulated rotation.
        Action::HorizontalScrollRight | Action::HorizontalScrollLeft => {
            dir.scroll += magnitude as f32 * scroll_multiplier(sensitivity);
            let lines = dir.scroll.trunc();
            if lines >= 1.0 {
                dir.scroll -= lines;
                let sign = if matches!(action, Action::HorizontalScrollRight) {
                    1
                } else {
                    -1
                };
                WheelOutput::Scroll(sign * lines as i32)
            } else {
                WheelOutput::Idle
            }
        }
        // Any other action: fire once per `action_threshold` increments, with
        // decay (forget stale partial progress) and cooldown (one flick = one
        // fire).
        _ => {
            if dir
                .last_event
                .is_some_and(|t| now.saturating_duration_since(t) > ACTION_DECAY)
            {
                dir.action = 0;
            }
            dir.last_event = Some(now);

            if dir
                .last_fired
                .is_some_and(|t| now.saturating_duration_since(t) < ACTION_COOLDOWN)
            {
                return WheelOutput::Idle;
            }

            dir.action += magnitude;
            if dir.action >= action_threshold(sensitivity) {
                dir.action = 0;
                dir.last_fired = Some(now);
                WheelOutput::FireAction
            } else {
                WheelOutput::Idle
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier_is_unity_at_default_sensitivity() {
        assert!((scroll_multiplier(DEFAULT_THUMBWHEEL_SENSITIVITY) - 1.0).abs() < f32::EPSILON);
        assert!(scroll_multiplier(DEFAULT_THUMBWHEEL_SENSITIVITY * 2) > 1.9);
        assert!(scroll_multiplier(1) < 0.1);
    }

    #[test]
    fn action_threshold_drops_with_sensitivity_and_floors_at_one() {
        assert_eq!(
            action_threshold(DEFAULT_THUMBWHEEL_SENSITIVITY),
            DEFAULT_THUMBWHEEL_SENSITIVITY
        );
        assert!(
            action_threshold(1) > action_threshold(DEFAULT_THUMBWHEEL_SENSITIVITY),
            "low sensitivity needs more increments"
        );
        assert_eq!(action_threshold(100), 1, "high sensitivity floors at one");
    }

    #[test]
    fn scroll_accumulates_fractionally_at_sub_unity_sensitivity() {
        let mut dir = WheelDirection::default();
        let now = Instant::now();
        // multiplier 0.5: two increments make one whole line.
        let half = DEFAULT_THUMBWHEEL_SENSITIVITY / 2;
        assert_eq!(
            advance(&mut dir, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Idle
        );
        assert_eq!(
            advance(&mut dir, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Scroll(1)
        );
    }

    #[test]
    fn scroll_left_emits_negative_lines() {
        let mut dir = WheelDirection::default();
        let now = Instant::now();
        assert_eq!(
            advance(
                &mut dir,
                &Action::HorizontalScrollLeft,
                1,
                DEFAULT_THUMBWHEEL_SENSITIVITY,
                now
            ),
            WheelOutput::Scroll(-1)
        );
    }

    #[test]
    fn directions_accumulate_independently() {
        // A reversal must not drain the other direction's pending progress.
        let mut up = WheelDirection::default();
        let mut down = WheelDirection::default();
        let now = Instant::now();
        let half = DEFAULT_THUMBWHEEL_SENSITIVITY / 2; // multiplier 0.5
        assert_eq!(
            advance(&mut up, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Idle
        );
        // One tick the other way doesn't cancel `up`'s banked half-line…
        assert_eq!(
            advance(&mut down, &Action::HorizontalScrollLeft, 1, half, now),
            WheelOutput::Idle
        );
        // …so `up`'s next tick still completes its own line.
        assert_eq!(
            advance(&mut up, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Scroll(1)
        );
    }

    #[test]
    fn custom_action_fires_on_threshold_then_respects_cooldown() {
        let mut dir = WheelDirection::default();
        let now = Instant::now();
        // Threshold at default sensitivity is DEFAULT increments.
        for _ in 0..DEFAULT_THUMBWHEEL_SENSITIVITY - 1 {
            assert_eq!(
                advance(
                    &mut dir,
                    &Action::VolumeUp,
                    1,
                    DEFAULT_THUMBWHEEL_SENSITIVITY,
                    now
                ),
                WheelOutput::Idle
            );
        }
        assert_eq!(
            advance(
                &mut dir,
                &Action::VolumeUp,
                1,
                DEFAULT_THUMBWHEEL_SENSITIVITY,
                now
            ),
            WheelOutput::FireAction
        );
        // Immediately after, the cooldown swallows further increments.
        for _ in 0..DEFAULT_THUMBWHEEL_SENSITIVITY {
            assert_eq!(
                advance(
                    &mut dir,
                    &Action::VolumeUp,
                    1,
                    DEFAULT_THUMBWHEEL_SENSITIVITY,
                    now
                ),
                WheelOutput::Idle
            );
        }
    }

    #[test]
    fn none_action_is_suppressed() {
        let mut dir = WheelDirection::default();
        assert_eq!(
            advance(
                &mut dir,
                &Action::None,
                5,
                DEFAULT_THUMBWHEEL_SENSITIVITY,
                Instant::now()
            ),
            WheelOutput::Idle
        );
    }

    #[test]
    fn rearms_when_the_current_session_dies_with_a_target() {
        // The live session ended on its own while a device is still targeted.
        assert!(should_rearm(7, 7, true));
    }

    #[test]
    fn ignores_a_stale_session_superseded_by_a_restart() {
        // An older session reports completion after a deliberate restart already
        // bumped the epoch; re-arming would needlessly cycle the live session.
        assert!(!should_rearm(6, 7, true));
    }

    #[test]
    fn ignores_a_deliberate_stop_to_idle() {
        // The session was stopped on purpose (pairing took the receiver, or no
        // device is targeted): no target means there is nothing to re-arm.
        assert!(!should_rearm(7, 7, false));
    }
}
