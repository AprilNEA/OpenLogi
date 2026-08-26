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
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use openlogi_core::config::ThumbwheelSensitivity;
use openlogi_core::scroll::ScrollDelta;
use openlogi_hid::session::gesture::{CaptureSpec, GESTURE_SOURCE_BUTTONS};
use openlogi_hid::{
    CaptureChannel, CapturedInput, ChannelRegistry, DeviceRoute,
    run_capture_session_with_registry_spec,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use self::dispatch::{InputDispatcher, WheelConfiguration};
use crate::capture_plan::{DeviceCapturePlan, SharedCapturePlans};
use crate::receiver_access::{ReceiverAccess, SessionReceiverLease};
use crate::runtime::scroll::ScrollInputHandle;
use crate::runtime::{ActionDispatcher, HidppSessionId};

/// How often to re-read the active device target + thumb-wheel arming so a
/// carousel switch or a binding/sensitivity edit re-points / re-arms capture.
/// It also paces the respawn of a session that ended on its own (see `manage`).
const TARGET_POLL: Duration = Duration::from_secs(1);

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
    registry: ChannelRegistry,
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
            registry,
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

/// Capture and wheel-state configuration that determines whether a session can
/// stay armed without leaking state across a binding epoch.
#[derive(Clone, PartialEq)]
struct SessionTarget {
    route: DeviceRoute,
    spec: CaptureSpec,
    wheel: WheelConfiguration,
    rearm_generation: u64,
}

impl SessionTarget {
    fn for_plan(plan: &DeviceCapturePlan) -> Self {
        Self {
            route: plan.route.clone(),
            spec: spec_for(plan),
            wheel: WheelConfiguration::for_plan(plan),
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
    registry: ChannelRegistry,
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
    registry: ChannelRegistry,
    outputs: GestureOutputs,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<CapturedEvent>();
    let mut sessions: HashMap<String, RunningSession> = HashMap::new();
    let mut ticker = tokio::time::interval(TARGET_POLL);
    let mut input_dispatcher = InputDispatcher::new(Arc::clone(&capture_plans), outputs);
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
        registry,
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
                    input_dispatcher.dispatch(&event.session, event.input);
                } else {
                    input_dispatcher.cancel_session(&event.session);
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
                        input_dispatcher.cancel_session(&session.id);
                        let _ = stop.send(());
                    }
                }
                input_dispatcher.retain_devices(|key| want.contains_key(key));
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
                    input_dispatcher.cancel_session(&done.session);
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
    let registry = channels.registry.clone();
    tokio::spawn(async move {
        let _lease = lease;
        if let Err(e) = run_capture_session_with_registry_spec(
            session_route,
            session_spec,
            session_tx,
            stop_rx,
            slot,
            &registry,
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

#[cfg(test)]
mod tests;
