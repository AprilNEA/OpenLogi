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
//!   continuous, sensitivity-scaled scroll or accumulated into a custom action,
//!
//! all via the common [`crate::runtime::ActionDispatcher`].
//!
//! Unlike the CGEventTap hook, this needs no macOS Accessibility permission —
//! the events arrive over HID++, and the bound action is synthesised the same
//! way regardless.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{Action, ButtonId, default_binding};
use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::session::gesture::{CaptureSpec, GESTURE_SOURCE_BUTTONS};
use openlogi_hid::{CaptureChannel, CapturedInput, DeviceRoute, run_capture_session};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::capture_plan::{DeviceCapturePlan, SharedCapturePlans};
use crate::receiver_access::{ReceiverAccess, SessionReceiverLease};
use crate::runtime::scroll::ScrollInputHandle;
use crate::runtime::{ActionDispatcher, HidppSessionId, PressToken};

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
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
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
            capture_channel,
            receiver_access,
            outputs,
        ));
    });
}

/// Whether one device's thumb wheel must be diverted over HID++ (which
/// suppresses native scroll) so we can re-synthesise its scroll or capture its
/// tap: its sensitivity leaves the default (so we scale scroll ourselves) or a
/// thumbwheel binding does.
fn thumbwheel_armed(plan: &DeviceCapturePlan) -> bool {
    plan.thumbwheel_sensitivity != ThumbwheelSensitivity::DEFAULT
        || plan.thumbwheel_bindings_nondefault
}

/// The [`CaptureSpec`] one device's session should run with right now.
fn spec_for(plan: &DeviceCapturePlan) -> CaptureSpec {
    CaptureSpec {
        capture_thumbwheel: thumbwheel_armed(plan),
        // Derived from the dispatch maps, so the armed diverts and the maps
        // resolving their events can never drift apart.
        divert_gesture_sources: GESTURE_SOURCE_BUTTONS
            .into_iter()
            .filter(|(_, button)| plan.gesture_bindings.contains_key(button))
            .map(|(cid, _)| cid)
            .collect(),
        divert_buttons: plan.divert_buttons.clone(),
    }
}

/// Capture configuration that determines whether a session can stay armed.
#[derive(Clone, PartialEq)]
struct SessionTarget {
    route: DeviceRoute,
    spec: CaptureSpec,
    rearm_generation: u64,
}

impl SessionTarget {
    fn for_plan(plan: &DeviceCapturePlan) -> Self {
        Self {
            route: plan.route.clone(),
            spec: spec_for(plan),
            rearm_generation: plan.rearm_generation,
        }
    }
}

/// One capture session tracked by the manager.
struct RunningSession {
    id: HidppSessionId,
    target: SessionTarget,
    /// Present while the session runs; taken to request a stop. `None` means
    /// the session is draining — deliberately stopped, but its task (and the
    /// control-restore writes in its teardown) may still be in flight.
    stop: Option<oneshot::Sender<()>>,
}

struct CapturedEvent {
    session: HidppSessionId,
    input: CapturedInput,
}

struct SessionDone {
    session: HidppSessionId,
}

#[derive(Clone)]
struct SessionChannels {
    inputs: mpsc::UnboundedSender<CapturedEvent>,
    done: mpsc::UnboundedSender<SessionDone>,
    capture: CaptureChannel,
}

/// Correlates completed HID++ gesture semantics with the exact physical press
/// token admitted by the shared button runtime. The runtime remains the sole
/// authority on whether the token is still active.
#[derive(Default)]
struct GesturePresses {
    tokens: HashMap<(HidppSessionId, ButtonId), PressToken>,
}

impl GesturePresses {
    fn start(&mut self, session: &HidppSessionId, button: ButtonId, press: PressToken) {
        self.tokens.insert((session.clone(), button), press);
    }

    fn get(&self, session: &HidppSessionId, button: ButtonId) -> Option<&PressToken> {
        self.tokens.get(&(session.clone(), button))
    }

    fn end(&mut self, session: &HidppSessionId, button: ButtonId) {
        self.tokens.remove(&(session.clone(), button));
    }

    fn cancel_session(&mut self, session: &HidppSessionId) {
        self.tokens.retain(|(candidate, _), _| candidate != session);
    }
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

/// Decide the [`DoneAction`] for a completion report carrying `done_session`,
/// given the session the manager currently tracks for that device (if any).
///
/// Only the *current* session's report settles anything; a stale epoch belongs
/// to a session already superseded. A tracked session whose stop sender is
/// gone was stopped deliberately and is merely draining — its report frees the
/// key quietly. One still holding its stop sender exited on its own and
/// warrants a warning alongside the re-arm.
fn on_done(done_session: &HidppSessionId, live: Option<&RunningSession>) -> DoneAction {
    match live {
        Some(session) if session.id == *done_session => DoneAction::Remove {
            unexpected: session.stop.is_some(),
        },
        _ => DoneAction::Ignore,
    }
}

/// Whether an input belongs to the current, still-live session. A draining
/// session has already emitted `Cancel`, so even its correctly-tagged queued
/// events must not enter the replacement lifecycle.
fn accepts_input(input_session: &HidppSessionId, live: Option<&RunningSession>) -> bool {
    live.is_some_and(|session| session.id == *input_session && session.stop.is_some())
}

/// Whether the plan currently published for a device still describes the
/// capture session that produced an input. This closes the interval between a
/// plan publication and the manager's next teardown tick.
fn session_matches_plan(session: &RunningSession, plan: &DeviceCapturePlan) -> bool {
    session.target == SessionTarget::for_plan(plan)
}

/// Snapshot the sessions that should be armed on this tick. Pairing owns the
/// receiver exclusively, so its request temporarily makes the wanted set
/// empty and lets the normal teardown path restore every control.
fn wanted_sessions(
    receiver_access: &ReceiverAccess,
    capture_plans: &SharedCapturePlans,
) -> HashMap<String, SessionTarget> {
    if receiver_access.exclusive_requested() {
        return HashMap::new();
    }
    capture_plans
        .read()
        .map(|plans| {
            plans
                .iter()
                .map(|plan| (plan.config_key.clone(), SessionTarget::for_plan(plan)))
                .collect()
        })
        .unwrap_or_default()
}

/// Keep one capture session alive per online device, restarting a session when
/// its device's plan changes, and dispatch incoming inputs against the plan of
/// the device they arrived on. Runs for the lifetime of the process.
async fn manage(
    capture_plans: SharedCapturePlans,
    capture_channel: CaptureChannel,
    receiver_access: ReceiverAccess,
    outputs: GestureOutputs,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<CapturedEvent>();
    let mut sessions: HashMap<String, RunningSession> = HashMap::new();
    let mut ticker = tokio::time::interval(TARGET_POLL);
    let mut accumulators: HashMap<String, WheelAccumulators> = HashMap::new();
    let mut gesture_presses = GesturePresses::default();
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed. Each session reports its completion here, tagged with its device
    // key and the epoch it started under: a dead *current* session re-arms on the
    // next tick, a deliberately stopped one merely frees its key for the
    // replacement once its teardown has drained, and stale completions are
    // ignored (see `on_done`).
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<SessionDone>();
    let channels = SessionChannels {
        inputs: tx,
        done: done_tx,
        capture: capture_channel,
    };
    let mut epoch: u64 = 0;
    // The capture-vs-pairing arbiter hands out one exclusive lease. All session
    // tasks share it through an `Arc`; the manager keeps only a `Weak` so the
    // lease frees itself when the last session exits (letting pairing proceed).
    let mut lease: std::sync::Weak<SessionReceiverLease> = std::sync::Weak::new();

    loop {
        tokio::select! {
            Some(event) = rx.recv() => {
                let key = event.session.device_key();
                let live = sessions.get(key);
                let current = accepts_input(&event.session, live)
                    && !receiver_access.exclusive_requested()
                    && capture_plans.read().is_ok_and(|plans| {
                        plans
                            .iter()
                            .find(|plan| plan.config_key == key)
                            .is_some_and(|plan| live.is_some_and(|session| session_matches_plan(session, plan)))
                    });
                if current {
                    dispatch(
                        &event.session,
                        event.input,
                        &mut accumulators,
                        &mut gesture_presses,
                        &capture_plans,
                        &outputs,
                    );
                } else {
                    outputs.cancel_session(&event.session);
                    gesture_presses.cancel_session(&event.session);
                    debug!(key, epoch = event.session.epoch(), "input from a stale capture session — ignored");
                }
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release every capture
                // session so run_pairing can own the receiver's HID node (one
                // process can't read it through two channels).
                let want = wanted_sessions(&receiver_access, &capture_plans);
                // Stop sessions whose device disappeared or whose plan changed.
                // Sending on the oneshot lets the session restore its controls.
                // A stopped session stays tracked — stop sender taken — until
                // its task reports completion below, and a tracked key is never
                // re-armed: arming the replacement while the old task may still
                // be mid-restore could interleave its divert writes with the
                // restore writes on the same device, leaving a control
                // un-diverted while the new session believes it owns it,
                // however many ticks the restore takes.
                for (key, session) in &mut sessions {
                    let keep = want.get(key).is_some_and(|target| *target == session.target);
                    if !keep && let Some(stop) = session.stop.take() {
                        outputs.cancel_session(&session.id);
                        gesture_presses.cancel_session(&session.id);
                        let _ = stop.send(());
                    }
                }
                accumulators.retain(|key, _| want.contains_key(key));
                for (key, target) in want {
                    if sessions.contains_key(&key) {
                        continue;
                    }
                    // All sessions share one exclusive lease; acquire it with the
                    // first session and ride the existing one afterwards.
                    let session_lease = if let Some(existing) = lease.upgrade() {
                        existing
                    } else {
                        let Some(fresh) = receiver_access.try_acquire_for_session() else {
                            continue;
                        };
                        let fresh = Arc::new(fresh);
                        lease = Arc::downgrade(&fresh);
                        fresh
                    };
                    epoch = epoch.wrapping_add(1);
                    let id = HidppSessionId::new(&key, epoch);
                    let session = spawn_session(
                        id,
                        target,
                        session_lease,
                        &channels,
                    );
                    sessions.insert(key, session);
                }
            }
            Some(done) = done_rx.recv() => {
                let key = done.session.device_key();
                // A capture session's task has fully exited — its restore writes
                // included — so dropping its entry lets the next tick start a
                // fresh session for that device; the tick fires at most once per
                // `TARGET_POLL`, which paces the respawn so a permanently failing
                // device can't hot-loop. A stale epoch (an already-superseded
                // session) is a no-op.
                if let DoneAction::Remove { unexpected } = on_done(&done.session, sessions.get(key)) {
                    outputs.cancel_session(&done.session);
                    gesture_presses.cancel_session(&done.session);
                    if unexpected {
                        warn!(key, "capture session ended unexpectedly, re-arming");
                    }
                    sessions.remove(key);
                }
            }
        }
    }
}

/// Start one device's capture session plus its input-forwarding task, and
/// return the manager's tracking entry for it.
fn spawn_session(
    id: HidppSessionId,
    target: SessionTarget,
    lease: Arc<SessionReceiverLease>,
    channels: &SessionChannels,
) -> RunningSession {
    let (stop_tx, stop_rx) = oneshot::channel();
    // Tag this session's inputs with its device key so dispatch resolves them
    // against the right plan.
    let (session_tx, mut session_rx) = mpsc::unbounded_channel::<CapturedInput>();
    let forward = channels.inputs.clone();
    let forward_id = id.clone();
    tokio::spawn(async move {
        while let Some(input) = session_rx.recv().await {
            let _ = forward.send(CapturedEvent {
                session: forward_id.clone(),
                input,
            });
        }
    });
    let done = channels.done.clone();
    let done_id = id.clone();
    let session_route = target.route.clone();
    let session_spec = target.spec.clone();
    let slot = Arc::clone(&channels.capture);
    tokio::spawn(async move {
        let _lease = lease;
        let backend = openlogi_hid::host::backend();
        if let Err(e) = run_capture_session(
            &*backend,
            session_route,
            session_spec,
            session_tx,
            stop_rx,
            slot,
        )
        .await
        {
            debug!(error = %e, "capture session ended");
        }
        // Report completion so the manager can re-arm if this exit was
        // unexpected rather than a deliberate stop.
        let _ = done.send(SessionDone { session: done_id });
    });
    RunningSession {
        id,
        target,
        stop: Some(stop_tx),
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
    /// Integer rotation-increment accumulator for a custom (non-scroll) action.
    action: i32,
    /// When the last rotation event for this direction arrived (decay clock).
    last_event: Option<Instant>,
    /// When this direction last fired its custom action (cooldown clock).
    last_fired: Option<Instant>,
}

/// What advancing a direction's accumulator should produce.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WheelOutput {
    /// Below threshold / suppressed — emit nothing.
    Idle,
    /// Post a typed fractional scroll distance.
    Scroll(ScrollDelta),
    /// Fire the direction's bound custom action.
    FireAction,
}

/// Route one captured input from `session` to its bound action (or
/// re-synthesised scroll), using that device's own plan maps.
fn dispatch(
    session: &HidppSessionId,
    input: CapturedInput,
    accumulators: &mut HashMap<String, WheelAccumulators>,
    gesture_presses: &mut GesturePresses,
    capture_plans: &SharedCapturePlans,
    outputs: &GestureOutputs,
) {
    let key = session.device_key();
    let Ok(plans) = capture_plans.read() else {
        return;
    };
    let Some(plan) = plans.iter().find(|plan| plan.config_key == key) else {
        debug!(key, "input from a device with no capture plan — ignored");
        return;
    };
    match input {
        CapturedInput::Gesture(button, direction) => {
            let Some(press) = gesture_presses.get(session, button) else {
                debug!(key, %button, ?direction, "gesture from a canceled button lifecycle — ignored");
                return;
            };
            if let Some(action) = plan
                .gesture_bindings
                .get(&button)
                .and_then(|map| map.get(&direction))
            {
                debug!(key, %button, ?direction, action = %action.label(), "gesture → action");
                if !outputs.actions.try_dispatch_while_pressed(press, action) {
                    debug!(key, %button, ?direction, "gesture press no longer active — ignored");
                }
            } else {
                debug!(key, %button, ?direction, "gesture with no binding — ignored");
            }
        }
        CapturedInput::ButtonDown(button) => {
            // A raw-XY gesture source owns its click/swipe map; its physical
            // lifecycle is still tracked, but it must not also fire the
            // single-action projection on down.
            let is_gesture = plan.gesture_bindings.contains_key(&button);
            let action = (!is_gesture).then(|| plan.bindings.get(&button)).flatten();
            if let Some(action) = action {
                debug!(key, ?button, action = %action.label(), "HID++ button → action");
            } else {
                debug!(key, ?button, "HID++ button with no binding — ignored");
            }
            let press = outputs
                .actions
                .try_hidpp_button_down(session, button, action);
            if is_gesture {
                if let Some(press) = press {
                    gesture_presses.start(session, button, press);
                } else {
                    gesture_presses.end(session, button);
                }
            }
        }
        CapturedInput::ButtonUp(button) => {
            outputs.actions.try_hidpp_button_up(session, button);
            gesture_presses.end(session, button);
        }
        CapturedInput::ButtonPulse(button) => {
            let action = plan.bindings.get(&button);
            if let Some(action) = action {
                debug!(key, ?button, action = %action.label(), "HID++ button pulse → action");
            } else {
                debug!(key, ?button, "HID++ button pulse with no binding — ignored");
            }
            outputs
                .actions
                .dispatch_hidpp_button_pulse(session, button, action);
        }
        CapturedInput::Scroll {
            increments,
            resolution,
        } => {
            // Positive rotation is "up"; each direction has its own binding.
            let up = increments >= 0;
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
            let magnitude = i32::from(increments).abs();
            match advance(
                dir,
                &action,
                magnitude,
                ScrollScale {
                    native_per_increment: resolution.native_per_increment(),
                    sensitivity,
                },
                Instant::now(),
            ) {
                WheelOutput::Idle => {}
                WheelOutput::Scroll(delta) => outputs.post_scroll(session, delta),
                WheelOutput::FireAction => {
                    debug!(key, ?button, action = %action.label(), "thumb wheel → action");
                    outputs.actions.dispatch(&action, Some(key));
                }
            }
        }
    }
}

/// How far one rotation increment should scroll.
#[derive(Debug, Clone, Copy)]
struct ScrollScale {
    /// Native scroll units one diverted increment is worth, from the wheel's
    /// own `getThumbwheelInfo`. Diverting the wheel changes the unit it
    /// reports in — an MX Master 4 goes from 20 ratchets per revolution to 120
    /// increments — so without this the same physical motion scrolls six times
    /// as far as it did natively, and the sensitivity slider's 1× is 1× of
    /// nothing recognisable.
    native_per_increment: f64,
    /// The user's own multiplier, relative to that native amount.
    sensitivity: ThumbwheelSensitivity,
}

impl ScrollScale {
    /// Scroll units one increment contributes.
    fn per_increment(self) -> f64 {
        self.native_per_increment * self.sensitivity.scroll_multiplier()
    }
}

/// Advance one direction's accumulator by `magnitude` rotation increments and
/// decide what to emit. Pure given `now`, so the decay/cooldown/threshold logic
/// is unit-testable without touching the OS.
fn advance(
    dir: &mut WheelDirection,
    action: &Action,
    magnitude: i32,
    scale: ScrollScale,
    now: Instant,
) -> WheelOutput {
    let sensitivity = scale.sensitivity;
    match action {
        // Suppressed: captured but produces nothing.
        Action::None => WheelOutput::Idle,
        // Continuous scroll, scaled from the wheel's diverted
        // increments back to its native amount and then by the user's
        // sensitivity. Direction comes from the action.
        Action::ScrollUp
        | Action::ScrollDown
        | Action::HorizontalScrollRight
        | Action::HorizontalScrollLeft => {
            let distance = f64::from(magnitude) * scale.per_increment();
            let delta = match action {
                Action::ScrollUp => ScrollDelta::wheel_ticks(0.0, distance),
                Action::ScrollDown => ScrollDelta::wheel_ticks(0.0, -distance),
                Action::HorizontalScrollRight => ScrollDelta::wheel_ticks(distance, 0.0),
                Action::HorizontalScrollLeft => ScrollDelta::wheel_ticks(-distance, 0.0),
                _ => unreachable!("scroll actions are matched above"),
            };
            if distance == 0.0 {
                WheelOutput::Idle
            } else {
                WheelOutput::Scroll(delta)
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
            if dir.action >= sensitivity.action_threshold() {
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
mod tests;
