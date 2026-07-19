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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, GestureDirection, default_binding};
use openlogi_core::config::DEFAULT_THUMBWHEEL_SENSITIVITY;
use openlogi_hid::gesture::CaptureSpec;
use openlogi_hid::{CaptureChannel, CapturedInput, DeviceRoute, run_capture_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::DpiCycles;
use crate::capture_plan::{DeviceCapturePlan, SharedCapturePlans};
use crate::hook_runtime;
use crate::receiver_access::{CaptureReceiverLease, ReceiverAccess};

/// Shared gesture-direction binding map, mirrored from `AppState` (keyed by
/// direction). The watcher reads it to map a captured swipe to a bound action.
pub type GestureBindings = Arc<RwLock<BTreeMap<GestureDirection, Action>>>;

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
    capture_plans: SharedCapturePlans,
    dpi_cycle: Arc<RwLock<DpiCycles>>,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
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
            dpi_cycle,
            capture_channel,
            receiver_access,
        ));
    });
}

/// Whether one device's thumb wheel must be diverted over HID++ (which
/// suppresses native scroll) so we can re-synthesise its scroll or capture its
/// tap: its sensitivity leaves the default (so we scale scroll ourselves) or a
/// thumbwheel binding does.
fn thumbwheel_armed(plan: &DeviceCapturePlan) -> bool {
    plan.thumbwheel_sensitivity != DEFAULT_THUMBWHEEL_SENSITIVITY
        || plan.thumbwheel_bindings_nondefault
}

/// The [`CaptureSpec`] one device's session should run with right now.
fn spec_for(plan: &DeviceCapturePlan) -> CaptureSpec {
    CaptureSpec {
        capture_thumbwheel: thumbwheel_armed(plan),
        divert_gesture_button: !plan.gesture_bindings.is_empty(),
        divert_buttons: plan.divert_buttons.clone(),
    }
}

/// One live capture session tracked by the manager.
struct RunningSession {
    route: DeviceRoute,
    spec: CaptureSpec,
    stop: Option<oneshot::Sender<()>>,
    epoch: u64,
}

/// What the manager should do with one session-completion report.
#[derive(Debug, PartialEq)]
enum DoneAction {
    /// A stale report from a session the manager no longer tracks — ignore it.
    Ignore,
    /// The tracked session's task has fully exited: drop its entry so the next
    /// tick may arm a successor. `unexpected` is true when the exit wasn't a
    /// deliberate stop and the drop deserves a warning.
    Remove { unexpected: bool },
}

/// Decide the [`DoneAction`] for a completion report carrying `done_epoch`,
/// given the session the manager currently tracks for that device (if any).
///
/// Only the *current* session's report settles anything; a stale epoch belongs
/// to a session already superseded by a deliberate restart. Deliberately
/// stopped sessions are dropped from the map at stop time, so any tracked
/// session's completion is an unexpected exit.
fn on_done(done_epoch: u64, live: Option<&RunningSession>) -> DoneAction {
    match live {
        Some(session) if session.epoch == done_epoch => DoneAction::Remove { unexpected: true },
        _ => DoneAction::Ignore,
    }
}

/// Keep one capture session alive per online device, restarting a session when
/// its device's plan changes, and dispatch incoming inputs against the plan of
/// the device they arrived on. Runs for the lifetime of the process.
async fn manage(
    capture_plans: SharedCapturePlans,
    dpi_cycle: Arc<RwLock<DpiCycles>>,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, CapturedInput)>();
    let mut sessions: HashMap<String, RunningSession> = HashMap::new();
    let mut ticker = tokio::time::interval(TARGET_POLL);
    let mut accumulators: HashMap<String, WheelAccumulators> = HashMap::new();
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed. Each session reports its completion here, tagged with its device
    // key and the epoch it started under, so a dead *current* session re-arms on
    // the next tick while stale completions are ignored.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(String, u64)>();
    let mut epoch: u64 = 0;
    // The capture-vs-pairing arbiter hands out one exclusive lease. All session
    // tasks share it through an `Arc`; the manager keeps only a `Weak` so the
    // lease frees itself when the last session exits (letting pairing proceed).
    let mut lease: std::sync::Weak<CaptureReceiverLease> = std::sync::Weak::new();

    loop {
        tokio::select! {
            Some((key, input)) = rx.recv() => {
                dispatch(
                    &key,
                    input,
                    &mut accumulators,
                    &capture_plans,
                    &dpi_cycle,
                    &capture_channel,
                );
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release every capture
                // session so run_pairing can own the receiver's HID node (one
                // process can't read it through two channels).
                let want: HashMap<String, (DeviceRoute, CaptureSpec)> =
                    if receiver_access.pairing_requested() {
                        HashMap::new()
                    } else {
                        capture_plans
                            .read()
                            .map(|plans| {
                                plans
                                    .iter()
                                    .map(|plan| {
                                        (
                                            plan.config_key.clone(),
                                            (plan.route.clone(), spec_for(plan)),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                // Stop sessions whose device disappeared or whose plan changed.
                // Sending on the oneshot lets the session restore its controls.
                // A key stopped this tick restarts on the *next* tick, never
                // this one: arming the replacement immediately could interleave
                // its divert writes with the old session's restore writes on
                // the same device, leaving a control un-diverted while the new
                // session believes it owns it.
                let mut stopping: HashSet<String> = HashSet::new();
                sessions.retain(|key, session| {
                    let keep = want
                        .get(key)
                        .is_some_and(|(route, spec)| *route == session.route && *spec == session.spec);
                    if !keep {
                        if let Some(stop) = session.stop.take() {
                            let _ = stop.send(());
                        }
                        stopping.insert(key.clone());
                    }
                    keep
                });
                accumulators.retain(|key, _| want.contains_key(key));
                for (key, (route, spec)) in want {
                    if sessions.contains_key(&key) || stopping.contains(&key) {
                        continue;
                    }
                    // All sessions share one exclusive lease; acquire it with the
                    // first session and ride the existing one afterwards.
                    let session_lease = if let Some(existing) = lease.upgrade() {
                        existing
                    } else {
                        let Some(fresh) = receiver_access.try_acquire_for_capture() else {
                            continue;
                        };
                        let fresh = Arc::new(fresh);
                        lease = Arc::downgrade(&fresh);
                        fresh
                    };
                    epoch = epoch.wrapping_add(1);
                    let session = spawn_session(
                        key.clone(),
                        route,
                        spec,
                        epoch,
                        session_lease,
                        &tx,
                        &done_tx,
                        &capture_channel,
                    );
                    sessions.insert(key, session);
                }
            }
            Some((key, done_epoch)) = done_rx.recv() => {
                // A capture session ended on its own. Dropping its entry lets the
                // next tick start a fresh session for that device; the tick fires
                // at most once per `TARGET_POLL`, which paces the respawn so a
                // permanently failing device can't hot-loop. A stale epoch (an
                // already-superseded session) is a no-op.
                if let DoneAction::Remove { unexpected } = on_done(done_epoch, sessions.get(&key)) {
                    if unexpected {
                        warn!(key, "capture session ended unexpectedly, re-arming");
                    }
                    sessions.remove(&key);
                }
            }
        }
    }
}

/// Start one device's capture session plus its input-forwarding task, and
/// return the manager's tracking entry for it.
#[allow(
    clippy::too_many_arguments,
    reason = "plumbing between the manager loop's channels; grouping them into \
              a struct would only relabel the same eight values"
)]
fn spawn_session(
    key: String,
    route: DeviceRoute,
    spec: CaptureSpec,
    epoch: u64,
    lease: Arc<CaptureReceiverLease>,
    inputs: &mpsc::UnboundedSender<(String, CapturedInput)>,
    done: &mpsc::UnboundedSender<(String, u64)>,
    capture_channel: &CaptureChannel,
) -> RunningSession {
    let (stop_tx, stop_rx) = oneshot::channel();
    // Tag this session's inputs with its device key so dispatch resolves them
    // against the right plan.
    let (session_tx, mut session_rx) = mpsc::unbounded_channel::<CapturedInput>();
    let forward = inputs.clone();
    let forward_key = key.clone();
    tokio::spawn(async move {
        while let Some(input) = session_rx.recv().await {
            let _ = forward.send((forward_key.clone(), input));
        }
    });
    let done = done.clone();
    let session_route = route.clone();
    let session_spec = spec.clone();
    let slot = Arc::clone(capture_channel);
    tokio::spawn(async move {
        let _lease = lease;
        if let Err(e) =
            run_capture_session(session_route, session_spec, session_tx, stop_rx, slot).await
        {
            debug!(error = %e, "capture session ended");
        }
        // Report completion so the manager can re-arm if this exit was
        // unexpected rather than a deliberate stop.
        let _ = done.send((key, epoch));
    });
    RunningSession {
        route,
        spec,
        stop: Some(stop_tx),
        epoch,
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

/// Route one captured input from device `key` to its bound action (or
/// re-synthesised scroll), using that device's own plan maps.
fn dispatch(
    key: &str,
    input: CapturedInput,
    accumulators: &mut HashMap<String, WheelAccumulators>,
    capture_plans: &SharedCapturePlans,
    dpi_cycle: &Arc<RwLock<DpiCycles>>,
    capture: &CaptureChannel,
) {
    let Ok(plans) = capture_plans.read() else {
        return;
    };
    let Some(plan) = plans.iter().find(|plan| plan.config_key == key) else {
        debug!(key, "input from a device with no capture plan — ignored");
        return;
    };
    match input {
        CapturedInput::Gesture(direction) => {
            if let Some(action) = plan.gesture_bindings.get(&direction) {
                debug!(key, ?direction, action = %action.label(), "gesture → action");
                hook_runtime::dispatch_action(action, dpi_cycle, Some(key), capture);
            } else {
                debug!(key, ?direction, "gesture with no binding — ignored");
            }
        }
        CapturedInput::ButtonPressed(button) => {
            if let Some(action) = plan.bindings.get(&button) {
                debug!(key, ?button, action = %action.label(), "HID++ button → action");
                hook_runtime::dispatch_action(action, dpi_cycle, Some(key), capture);
            } else {
                debug!(key, ?button, "HID++ button with no binding — ignored");
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
            let action = plan
                .bindings
                .get(&button)
                .cloned()
                .unwrap_or_else(|| default_binding(button));
            let sensitivity = plan.thumbwheel_sensitivity;
            let wheels = accumulators.entry(key.to_owned()).or_default();
            let dir = if up { &mut wheels.up } else { &mut wheels.down };
            let magnitude = i32::from(rotation).abs();
            match advance(dir, &action, magnitude, sensitivity, Instant::now()) {
                WheelOutput::Idle => {}
                WheelOutput::Scroll(lines) => {
                    openlogi_inject::post_horizontal_scroll(lines);
                }
                WheelOutput::FireAction => {
                    debug!(key, ?button, action = %action.label(), "thumb wheel → action");
                    hook_runtime::dispatch_action(&action, dpi_cycle, Some(key), capture);
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

    /// A session whose stop sender is already gone (taken by a deliberate stop).
    fn stopped_session_with_epoch(epoch: u64) -> RunningSession {
        RunningSession {
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc548,
            },
            spec: CaptureSpec::default(),
            stop: None,
            epoch,
        }
    }

    /// A session still holding its stop sender (never asked to stop).
    fn live_session_with_epoch(epoch: u64) -> RunningSession {
        let (stop, _rx) = oneshot::channel();
        RunningSession {
            stop: Some(stop),
            ..stopped_session_with_epoch(epoch)
        }
    }

    #[test]
    fn rearms_when_the_current_session_dies() {
        // The live session for this device ended on its own.
        assert_eq!(
            on_done(7, Some(&live_session_with_epoch(7))),
            DoneAction::Remove { unexpected: true }
        );
    }

    #[test]
    fn ignores_a_stale_session_superseded_by_a_restart() {
        // An older session reports completion after a deliberate restart already
        // bumped the epoch; re-arming would needlessly cycle the live session.
        assert_eq!(
            on_done(6, Some(&live_session_with_epoch(7))),
            DoneAction::Ignore
        );
    }

    #[test]
    fn ignores_a_completion_for_an_untracked_device() {
        // The session's entry is already gone (a deliberate stop to idle, or a
        // device that went away): there is nothing to settle or re-arm.
        assert_eq!(on_done(7, None), DoneAction::Ignore);
    }

    #[test]
    #[ignore = "drain-until-done lands with the fix commit"]
    fn settles_a_draining_session_quietly() {
        // A deliberately stopped session stays tracked until its task — the
        // control-restore writes included — actually exits, so its key cannot
        // re-arm mid-restore. Its completion report frees the key without the
        // unexpected-exit warning.
        assert_eq!(
            on_done(7, Some(&stopped_session_with_epoch(7))),
            DoneAction::Remove { unexpected: false }
        );
    }
}
