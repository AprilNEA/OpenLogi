//! Flow: cursor-at-screen-edge host switching.
//!
//! Polls the global cursor position while Flow is armed, feeds the pure
//! `edge::EdgeStateMachine`, and applies a fired switch through the same
//! exclusive-receiver `ChangeHost` transition the keyboard host keys use. The
//! computer on the other side runs its own OpenLogi and brings the devices
//! back from its opposite edge — no network is involved; the devices
//! physically re-pair their wireless channel.
//!
//! Latency shape: the moment the cursor *enters* a mapped zone the watcher
//! starts acquiring the exclusive receiver lease, which (via
//! `ReceiverAccess::subscribe_requests`) wakes every capture watcher to release
//! immediately — so session teardown overlaps the short dwell instead of
//! serializing after it, and the fire itself only waits for the HID++ writes.

pub(crate) mod edge;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::config::FlowTriggerMode;
use openlogi_hid::{
    ChannelPool, DeviceIoGate, DeviceRoute, PreparedHostSwitch, prepare_host_switch,
    switch_linked_hosts, switch_linked_hosts_strict,
};
use openlogi_hook::DisplayBounds;
use tokio::sync::{oneshot, watch};
use tracing::{debug, info, warn};

use super::shutdown::{ManagerCompletion, WatcherHandle};
use crate::receiver_access::{ExclusiveAccessReason, ExclusiveReceiverLease, ReceiverAccess};
use edge::{EdgeObservation, EdgeStateMachine, ZoneTrigger};

pub use edge::triggers_for;

/// Cursor sampling cadence while Flow is armed (~60 Hz).
const ACTIVE_POLL: Duration = Duration::from_millis(16);

/// Re-check cadence while Flow is off or has no device to move — the watcher
/// costs one `RwLock` read per second until it arms.
const IDLE_POLL: Duration = Duration::from_secs(1);

/// How often the display list is re-read while armed. Geometry changes are
/// rare; the per-tick work stays one cursor read and pure math.
const DISPLAY_REFRESH: Duration = Duration::from_secs(2);

/// Anti-graze dwell — a few poll ticks, so brushing an edge on the way to a
/// scrollbar doesn't switch. Effectively instant to a deliberate push; the
/// receiver-lease acquisition it overlaps takes longer anyway.
const DWELL: Duration = Duration::from_millis(50);

/// Anti-bounce between fires. The spent latch (no refire until the cursor
/// leaves the zone) is the primary guard; this only debounces a jittery
/// cursor rattling across a zone boundary, so it stays short enough to
/// never be felt in a deliberate round trip.
const COOLDOWN: Duration = Duration::from_millis(250);

/// Pacing between attempts to (re)prepare the switch — a device that was
/// asleep at arm time gets re-probed on this cadence, not per poll tick.
const PREPARE_RETRY: Duration = Duration::from_secs(5);

/// Everything one armed Flow needs, resolved by the orchestrator from config
/// + inventory — the watcher itself never reads either.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowSpec {
    /// The zone → host bindings, pre-expanded from the pointer's side
    /// placements via [`triggers_for`].
    pub triggers: Vec<ZoneTrigger>,
    /// What arms an edge: plain contact, or contact while Ctrl is held.
    pub trigger_mode: FlowTriggerMode,
    /// The pointing device that jumps hosts. `switch_linked_hosts` validates
    /// its move first and applies it last.
    pub pointer: DeviceRoute,
    /// Devices that follow the pointer, resolved from each device's
    /// `flow_follow` setting.
    pub followers: Vec<DeviceRoute>,
}

/// Shared resolved spec; `None` while Flow is disabled, unmapped, or has no
/// online pointing device. A read-only projection published by the
/// orchestrator, like every other runtime spec.
pub type SharedFlowSpec = watch::Receiver<Option<Arc<FlowSpec>>>;

/// Everything the Flow manager needs from the shared runtime.
struct FlowManagerContext {
    spec: SharedFlowSpec,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
    shutdown: oneshot::Receiver<()>,
}

/// Spawn the Flow watcher.
#[must_use]
pub fn spawn(
    spec: &SharedFlowSpec,
    channel_pool: ChannelPool,
    receiver_access: ReceiverAccess,
    device_io: DeviceIoGate,
) -> WatcherHandle {
    let spec = spec.clone();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (shutdown_done_tx, shutdown_done_rx) = oneshot::channel();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "flow watcher: could not build tokio runtime");
                let _ = shutdown_done_tx.send(ManagerCompletion::Unexpected);
                return;
            }
        };
        let completion = runtime.block_on(watch(FlowManagerContext {
            spec,
            channel_pool,
            receiver_access,
            device_io,
            shutdown: shutdown_rx,
        }));
        // The fire path spawns the lease acquisition; destroy the runtime
        // before reporting that no Flow task can still write a device.
        drop(runtime);
        let _ = shutdown_done_tx.send(completion);
    });
    WatcherHandle::new(shutdown_tx, shutdown_done_rx)
}

/// The pre-acquired exclusive lease's lifecycle, driven by the edge machine:
/// `Pending` starts an acquisition, `Fire` consumes it, `Idle` withdraws it.
/// The acquisition task keeps the lease once acquired; there is no separate
/// held state — the fire simply awaits the (usually already finished) task.
enum LeaseState {
    /// Nothing requested.
    Idle,
    /// `acquire_exclusive` is running on a task; aborting (or dropping the
    /// completed handle) withdraws the request / releases the lease.
    Acquiring(tokio::task::JoinHandle<ExclusiveReceiverLease>),
}

impl LeaseState {
    /// Withdraw whatever is outstanding. Dropping an unfinished acquisition
    /// task cancels it (the request bit clears via `ExclusiveRequest::drop`);
    /// dropping a finished handle drops the lease it produced.
    fn release(&mut self) {
        if let Self::Acquiring(handle) = std::mem::replace(self, Self::Idle) {
            handle.abort();
        }
    }

    /// Resolve to a held lease for the fire, acquiring from scratch if the
    /// pre-acquisition never started (a zero-dwell entry can fire on its
    /// first observed tick).
    async fn take_for_fire(&mut self, receiver_access: &ReceiverAccess) -> ExclusiveReceiverLease {
        match std::mem::replace(self, Self::Idle) {
            Self::Acquiring(handle) => match handle.await {
                Ok(lease) => lease,
                // The acquire task can only end early by abort/panic; fall
                // back to a fresh acquisition.
                Err(_) => {
                    receiver_access
                        .acquire_exclusive(ExclusiveAccessReason::HostTransition)
                        .await
                }
            },
            Self::Idle => {
                receiver_access
                    .acquire_exclusive(ExclusiveAccessReason::HostTransition)
                    .await
            }
        }
    }
}

async fn watch(context: FlowManagerContext) -> ManagerCompletion {
    let FlowManagerContext {
        spec,
        channel_pool,
        receiver_access,
        device_io,
        mut shutdown,
    } = context;
    let mut machine = EdgeStateMachine::default();
    let mut displays: Vec<DisplayBounds> = Vec::new();
    let mut displays_read_at: Option<Instant> = None;
    let mut lease = LeaseState::Idle;
    let mut prepared: Option<PreparedFlow> = None;
    let mut prepare_attempted_at: Option<Instant> = None;
    loop {
        // Disarmed: drop all zone state and any outstanding lease, and stop
        // touching the cursor entirely. A suspended host counts as disarmed —
        // proactive HID++ probes and fires both wait for the resume. The
        // prepared cache is deliberately KEPT — the devices being away is
        // exactly the state a return-trip re-arm resumes from, and
        // re-preparing would put the slow path back on the first push after
        // they return. Its receiver channel stays pooled either way, and a
        // transport that genuinely died falls back at fire time.
        let armed = read_spec(&spec).filter(|_| device_io.allows_io());
        let Some(current) = armed else {
            machine = EdgeStateMachine::default();
            displays_read_at = None;
            lease.release();
            prepare_attempted_at = None;
            if !nap(IDLE_POLL, &mut shutdown).await {
                return ManagerCompletion::Graceful;
            }
            continue;
        };
        let now = Instant::now();
        // Resolve the switch ahead of the fire: transports open, feature
        // indices and slot verdicts read, so a fire is one write per device.
        // Retries are paced — a device asleep at arm time shouldn't be
        // hammered with probes every 16 ms.
        if prepared.as_ref().is_none_or(|cache| cache.spec != current)
            && prepare_attempted_at
                .is_none_or(|attempted| now.duration_since(attempted) >= PREPARE_RETRY)
            && !receiver_access.exclusive_requested()
        {
            prepare_attempted_at = Some(now);
            prepared = prepare_flow(&current, &channel_pool).await;
        }
        if displays_read_at.is_none_or(|read_at| now.duration_since(read_at) >= DISPLAY_REFRESH) {
            displays = openlogi_hook::displays();
            displays_read_at = Some(now);
        }
        let observation = match openlogi_hook::cursor_position() {
            Some(point) => machine.observe(
                point,
                &displays,
                effective_triggers(
                    current.trigger_mode,
                    openlogi_hook::control_held(),
                    &current.triggers,
                ),
                DWELL,
                COOLDOWN,
                now,
            ),
            None => EdgeObservation::Idle,
        };
        match observation {
            EdgeObservation::Idle => lease.release(),
            EdgeObservation::Pending { .. } => {
                // A fire is imminent: start tearing capture sessions down now
                // so the receiver is already exclusive when the dwell ends.
                // Pairing wins outright — while it is waiting or active, don't
                // even queue behind it.
                if matches!(lease, LeaseState::Idle) {
                    if receiver_access.requested(ExclusiveAccessReason::Pairing) {
                        debug!("flow: edge pending during pairing — not arming");
                    } else {
                        let access = receiver_access.clone();
                        lease = LeaseState::Acquiring(tokio::spawn(async move {
                            access
                                .acquire_exclusive(ExclusiveAccessReason::HostTransition)
                                .await
                        }));
                    }
                }
            }
            EdgeObservation::Fire { host } => {
                if fire(
                    &current,
                    host,
                    prepared.as_ref(),
                    &channel_pool,
                    &receiver_access,
                    &mut lease,
                )
                .await
                {
                    prepared = None;
                    prepare_attempted_at = None;
                }
            }
        }
        if !nap(ACTIVE_POLL, &mut shutdown).await {
            lease.release();
            return ManagerCompletion::Graceful;
        }
    }
}

/// Apply one fired edge. Reports whether the prepared cache was spent — a
/// caller that sees `true` drops it (and its retry pacing) so the next tick
/// re-resolves the switch from scratch.
async fn fire(
    current: &FlowSpec,
    host: u8,
    prepared: Option<&PreparedFlow>,
    channel_pool: &ChannelPool,
    receiver_access: &ReceiverAccess,
    lease: &mut LeaseState,
) -> bool {
    if receiver_access.requested(ExclusiveAccessReason::Pairing) {
        // Drop the fire rather than queue a switch behind pairing. The machine
        // stays spent until the cursor leaves the zone, so nothing fires twice
        // once the receiver frees up.
        debug!("flow: edge fired during pairing — ignored");
        lease.release();
        return false;
    }
    let held = lease.take_for_fire(receiver_access).await;
    let outcome = match prepared {
        Some(cache) if cache.spec == *current => fast_switch(cache, host).await,
        _ => FastOutcome::AbortedCleanly,
    };
    match outcome {
        FastOutcome::Switched => {
            drop(held);
            false
        }
        // No cache, a stale cache, or a failure before any write was accepted:
        // nothing has moved, so the strict all-or-nothing path is safe.
        // Invalidate and re-resolve.
        FastOutcome::AbortedCleanly => {
            switch(current, host, channel_pool, held).await;
            true
        }
        // Past the commit point: a follower already departed, so aborting would
        // split the set. Press forward tolerantly to bring the pointer (and
        // whoever is still here) across to it.
        FastOutcome::Committed => {
            switch_forward(current, host, channel_pool, held).await;
            true
        }
    }
}

/// Wait out one poll interval, or report `false` when the process asked the
/// manager to stop first. Every wait in the loop goes through here so a stop
/// request is never held up by a full idle interval.
async fn nap(delay: Duration, shutdown: &mut oneshot::Receiver<()>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(delay) => true,
        _ = shutdown => false,
    }
}

/// The triggers the machine may act on this tick. In [`FlowTriggerMode::CtrlEdge`]
/// an edge is only live while a Ctrl key is provably held — an unknown state
/// (`None`) stays inert so the mode never degrades to plain-edge behavior on
/// a platform that cannot answer.
fn effective_triggers(
    mode: FlowTriggerMode,
    ctrl_held: Option<bool>,
    triggers: &[ZoneTrigger],
) -> &[ZoneTrigger] {
    match mode {
        FlowTriggerMode::Edge => triggers,
        FlowTriggerMode::CtrlEdge if ctrl_held == Some(true) => triggers,
        FlowTriggerMode::CtrlEdge => &[],
    }
}

fn read_spec(spec: &SharedFlowSpec) -> Option<FlowSpec> {
    spec.borrow().as_deref().cloned()
}

/// The switch resolved ahead of time for the currently armed spec: every
/// device's transport open, `ChangeHost` located, and slot verdicts read, so
/// a fire is a single write per device (see `PreparedHostSwitch`).
struct PreparedFlow {
    /// The spec this cache was built for; a changed spec invalidates it.
    spec: FlowSpec,
    pointer: PreparedHostSwitch,
    followers: Vec<PreparedHostSwitch>,
}

/// Resolve `spec` into a [`PreparedFlow`]. All-or-nothing: a device that
/// fails to prepare (asleep, mid-reconnect) aborts the whole cache, so the
/// paced retry keeps trying and fires meanwhile take the full re-resolving
/// path. A cache silently missing a follower would fast-fire the pointer and
/// leave that follower behind on this host — the failure mode Flow exists to
/// prevent. The spec's followers are all online (the orchestrator filters),
/// so a persistent refusal here is a transient radio problem, not a device
/// that will never answer.
async fn prepare_flow(spec: &FlowSpec, channel_pool: &ChannelPool) -> Option<PreparedFlow> {
    let hosts: Vec<u8> = spec.triggers.iter().map(|trigger| trigger.host).collect();
    let pointer = match prepare_host_switch(&spec.pointer, &hosts, channel_pool).await {
        Ok(prepared) => prepared,
        Err(error) => {
            debug!(%error, route = %spec.pointer, "flow: pointer did not prepare — will retry");
            return None;
        }
    };
    let mut followers = Vec::with_capacity(spec.followers.len());
    for route in &spec.followers {
        match prepare_host_switch(route, &hosts, channel_pool).await {
            Ok(prepared) => followers.push(prepared),
            Err(error) => {
                debug!(%error, route = %route, "flow: follower did not prepare — will retry");
                return None;
            }
        }
    }
    debug!(
        route = %spec.pointer,
        followers = followers.len(),
        "flow: switch prepared — fires are now single writes"
    );
    Some(PreparedFlow {
        spec: spec.clone(),
        pointer,
        followers,
    })
}

/// How a prepared fire ended, and therefore which fallback is safe. The
/// fire-and-forget writes are a commit point: an accepted follower write
/// cannot be recalled, so everything after the first one must converge on
/// the target host rather than abort.
enum FastOutcome {
    /// Every device switched (or was already on the target host).
    Switched,
    /// Nothing was told to move; the strict all-or-nothing fallback is safe.
    AbortedCleanly,
    /// At least one follower departed before a later failure. Aborting now
    /// would split the set — the caller must press forward instead.
    Committed,
}

/// The prepared fire: one pre-validated write per follower, then the pointer
/// last (once the pointer leaves, nothing else can be commanded through a
/// shared receiver). A failure before any write is accepted aborts cleanly;
/// after one, the remaining followers are still attempted (they can only
/// converge) and the outcome reports the commit so the caller pushes the
/// pointer forward rather than stranding it behind a departed follower.
async fn fast_switch(prepared: &PreparedFlow, host: u8) -> FastOutcome {
    info!(host, route = %prepared.spec.pointer, "flow: edge trigger fired (prepared)");
    let mut departed = false;
    for follower in &prepared.followers {
        match follower.switch_to(host).await {
            Ok(switched) => departed |= switched,
            Err(error) if !departed => {
                debug!(%error, host, "flow: prepared follower switch failed — falling back");
                return FastOutcome::AbortedCleanly;
            }
            Err(error) => {
                warn!(%error, host, "flow: follower failed after another departed — pressing on");
            }
        }
    }
    match prepared.pointer.switch_to(host).await {
        Ok(true) => {
            info!(host, route = %prepared.spec.pointer, "flow: devices switched host");
            FastOutcome::Switched
        }
        Ok(false) => {
            debug!(host, route = %prepared.spec.pointer, "flow: device already on the requested host");
            FastOutcome::Switched
        }
        Err(error) if departed => {
            warn!(%error, host, "flow: pointer write failed after a follower departed");
            FastOutcome::Committed
        }
        Err(error) => {
            debug!(%error, host, "flow: prepared pointer switch failed — falling back");
            FastOutcome::AbortedCleanly
        }
    }
}

/// Apply the switch under the already-acquired exclusive lease, then release
/// it immediately so capture re-arms while the device departs. Only safe
/// while nothing has moved yet — the caller guarantees it.
///
/// All-or-nothing: a follower that cannot come along keeps the pointer here
/// too, rather than splitting the set across two computers. The abort is
/// self-healing — its own HID++ traffic wakes a sleeping follower, and the
/// spent latch means the user's next edge push retries the whole set — but
/// it reads as a dead edge push in the moment, so it warns rather than
/// debug-logs.
async fn switch(
    spec: &FlowSpec,
    host: u8,
    channel_pool: &ChannelPool,
    lease: ExclusiveReceiverLease,
) {
    info!(host, route = %spec.pointer, "flow: edge trigger fired");
    match switch_linked_hosts_strict(&spec.pointer, &spec.followers, host, channel_pool).await {
        Ok(true) => info!(host, route = %spec.pointer, "flow: devices switched host"),
        Ok(false) => {
            debug!(host, route = %spec.pointer, "flow: device already on the requested host");
        }
        Err(error) => {
            warn!(%error, route = %spec.pointer, host, "flow: switch aborted — no device moved");
        }
    }
    drop(lease);
}

/// The past-the-commit-point recovery: a follower has already departed, so
/// converging on the target host is the only move that reunites the set.
/// Re-resolves through the *tolerant* transition — a departed follower fails
/// its prepare and is skipped (it is already there), anything else still
/// reachable comes along, and the pointer moves regardless.
async fn switch_forward(
    spec: &FlowSpec,
    host: u8,
    channel_pool: &ChannelPool,
    lease: ExclusiveReceiverLease,
) {
    match switch_linked_hosts(&spec.pointer, &spec.followers, host, channel_pool).await {
        Ok(_) => {
            info!(host, route = %spec.pointer, "flow: pointer pushed through to the departed followers");
        }
        Err(error) => {
            warn!(%error, route = %spec.pointer, host, "flow: pointer could not follow its departed followers");
        }
    }
    drop(lease);
}

#[cfg(test)]
mod tests {
    use super::*;
    use edge::triggers_for;
    use openlogi_core::config::{FlowPlacements, FlowSide};

    fn some_triggers() -> Vec<ZoneTrigger> {
        let mut placements = FlowPlacements::default();
        placements.set(FlowSide::Right, Some(1));
        triggers_for(&placements)
    }

    #[test]
    fn plain_edge_mode_ignores_ctrl_state() {
        let triggers = some_triggers();
        for ctrl in [None, Some(false), Some(true)] {
            assert_eq!(
                effective_triggers(FlowTriggerMode::Edge, ctrl, &triggers),
                triggers.as_slice()
            );
        }
    }

    #[test]
    fn ctrl_edge_mode_requires_a_provably_held_ctrl() {
        let triggers = some_triggers();
        assert_eq!(
            effective_triggers(FlowTriggerMode::CtrlEdge, Some(true), &triggers),
            triggers.as_slice()
        );
        assert!(effective_triggers(FlowTriggerMode::CtrlEdge, Some(false), &triggers).is_empty());
        // Unknown (Wayland) must not degrade CtrlEdge into plain Edge.
        assert!(effective_triggers(FlowTriggerMode::CtrlEdge, None, &triggers).is_empty());
    }
}
