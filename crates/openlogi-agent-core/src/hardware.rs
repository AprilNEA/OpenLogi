//! Hardware-side actions invoked from both the GPUI thread (slider release)
//! and the OS-event hook thread (bound button press).
//!
//! [`DeviceOp`] is the seam every device write and read goes through: it binds
//! a [`DeviceRoute`] to the agent's capture/inventory channels (built via
//! [`crate::orchestrator::SharedHandles::device`] or
//! [`crate::orchestrator::SharedHandles::keyboard_device`]), then either
//! awaits [`DeviceOp::run`] (the IPC server's reads/writes, which must report
//! their result to the GUI) or fires [`DeviceOp::detach`] (the OS-hook and
//! reconnect paths, which must never block their caller). Both resolve the
//! channel the same way every caller always did — a registry-confirmed
//! capture channel or the exact current inventory channel; a registry miss is
//! unavailable, never a fallback to re-enumerating and opening a competing
//! connection.
//!
//! `detach` spawns a one-shot tokio runtime on a dedicated OS thread — cheap
//! at the cadence these fire at (≤ once per slider release / button press)
//! and avoids holding a long-lived async runtime alongside GPUI's executor.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use openlogi_core::config::Lighting;
use openlogi_hid::{
    CaptureChannelSlot, ChannelRegistry, DeviceIoGate, DeviceRoute, Dpi, HidppOperation, HostOs,
    OsLayoutOutcome, ReportRate, ScrollResolution, SharedChannel, SmartShiftStatus,
    WeakSharedChannel, WriteError,
};
use tokio::time::error::Elapsed;
use tracing::{debug, info, warn};

use crate::capture_plan::SharedCapturePlans;
use crate::receiver_access::ReceiverAccess;

mod context;
mod light;
mod rgb_control;
mod write_order;

pub use context::HardwareContext;
pub(crate) use write_order::{WriteOrder, WriteTicket};

/// Upper bound on a single HID++ write. `hidpp` has no request timeout of its
/// own, so without this an asleep / unresponsive device would hang (and leak)
/// this background thread forever; a write to a live device completes in well
/// under a second.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Physical transport behind a route. Slots on one receiver share a HID writer,
/// so they share one turnstile even though their device indices differ.
#[derive(Clone, PartialEq, Eq)]
enum TransportKey {
    Bolt(String),
    Unifying(String),
    Direct(u16, u16),
    RawHid(u16, u16, u16, u16, String),
}

impl From<&DeviceRoute> for TransportKey {
    fn from(route: &DeviceRoute) -> Self {
        match route {
            DeviceRoute::Bolt { receiver_uid, .. } => Self::Bolt(receiver_uid.clone()),
            DeviceRoute::Unifying { receiver_uid, .. } => Self::Unifying(receiver_uid.clone()),
            DeviceRoute::Direct {
                vendor_id,
                product_id,
            } => Self::Direct(*vendor_id, *product_id),
            DeviceRoute::RawHid {
                vendor_id,
                product_id,
                usage_page,
                usage_id,
                identity,
            } => Self::RawHid(
                *vendor_id,
                *product_id,
                *usage_page,
                *usage_id,
                identity.clone(),
            ),
        }
    }
}

type TransportTurn = tokio::sync::Mutex<()>;

struct TransportEntry {
    key: TransportKey,
    turn: Weak<TransportTurn>,
}

/// Per-transport FIFO turns for agent-owned HID requests.
///
/// Callers used to start the device-response timeout before the HID writer
/// was free. A slow wheel reconciliation could then spend a queued SmartShift
/// or DPI write's whole budget, including the round trip that confirms the
/// device's reply. Waiting for the turn happens before that budget starts.
#[derive(Clone, Default)]
pub(crate) struct TransportTurns {
    inner: Arc<Mutex<Vec<TransportEntry>>>,
}

impl TransportTurns {
    fn turn_for(&self, route: &DeviceRoute) -> Arc<TransportTurn> {
        let key = TransportKey::from(route);
        let mut entries = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|entry| entry.turn.strong_count() != 0);
        if let Some(turn) = entries
            .iter()
            .find_map(|entry| (entry.key == key).then(|| entry.turn.upgrade()).flatten())
        {
            return turn;
        }
        let turn = Arc::new(TransportTurn::new(()));
        entries.push(TransportEntry {
            key,
            turn: Arc::downgrade(&turn),
        });
        turn
    }
}

/// Run `work` only after `turn` is held, so a timeout inside `work` measures
/// the device transaction and not the queue ahead of it.
async fn after_transport_turn<T>(turn: &TransportTurn, work: impl Future<Output = T>) -> T {
    let _queued = turn.lock().await;
    work.await
}

/// Resolve `op`'s current channel only after `turn` is held, then give `write`
/// the whole [`WRITE_TIMEOUT`]. A channel cloned before the wait would pin a
/// connection inventory has already retired.
async fn timed_on_current_channel<T, F, Fut>(
    op: &DeviceOp,
    turn: &TransportTurn,
    write: F,
) -> Result<Result<T, WriteError>, Elapsed>
where
    F: FnOnce(SharedChannel) -> Fut,
    Fut: Future<Output = Result<T, WriteError>>,
{
    after_transport_turn(turn, async move {
        match op.resolve() {
            Ok(shared) => tokio::time::timeout(WRITE_TIMEOUT, write(shared)).await,
            Err(_) => Ok(Err(WriteError::DeviceNotFound)),
        }
    })
    .await
}

/// Upper bound on waiting, at exit, for lighting writes already running to
/// record the `0x8071` control they take. A write is a handful of requests;
/// the budget only matters when a device stopped answering.
const RGB_SETTLE_BUDGET: Duration = Duration::from_secs(2);

/// Upper bound on handing `0x8071` software control back while the agent
/// exits. One release is two requests; the budget only matters when a device
/// stopped answering.
const RGB_RELEASE_BUDGET: Duration = Duration::from_secs(2);

/// Select the only Agent-authoritative channel for `route`.
fn authoritative_channel(
    capture: Option<&CaptureChannelSlot>,
    registry: &ChannelRegistry,
    route: &DeviceRoute,
) -> Result<SharedChannel, WriteError> {
    let capture = capture
        .and_then(|capture| capture.read().ok())
        .and_then(|slot| (*slot).clone())
        .filter(|channel| channel.matches(route));
    choose_authoritative(
        capture,
        |channel| registry.is_current(channel),
        || registry.lookup(route),
    )
    .ok_or(WriteError::DeviceNotFound)
}

/// Hand every `0x8071` lease this session's lighting writes took back to the
/// firmware, as G HUB and Solaar do on exit, so the device returns to its
/// onboard effect and another host can take it. New lighting writes are
/// refused first, and the ones already running get [`RGB_SETTLE_BUDGET`] to
/// record their leases, so a write that claims during the exit is still
/// handed back. Releases then run in parallel within [`RGB_RELEASE_BUDGET`];
/// an absent device or failed release is logged, never allowed to hold up
/// the exit.
pub(crate) async fn release_session_rgb_control(access: &DeviceAccess) {
    let settle = tokio::time::Instant::now() + RGB_SETTLE_BUDGET;
    let jobs: Vec<_> = rgb_control::SESSION
        .close(settle)
        .await
        .into_iter()
        .filter_map(|(route, connection, lease)| {
            match access.op(&route).release_rgb_control(connection, lease) {
                Ok(job) => Some((route, job)),
                Err(error) => {
                    warn!(%route, ?error, "could not start the RGB control release");
                    None
                }
            }
        })
        .collect();
    let deadline = tokio::time::Instant::now() + RGB_RELEASE_BUDGET;
    for (route, job) in jobs {
        match tokio::time::timeout_at(deadline, job.wait()).await {
            Ok(Ok(())) => debug!(%route, "returned RGB control to the firmware"),
            Ok(Err(error)) => debug!(%route, ?error, "RGB control release skipped"),
            Err(_) => warn!(%route, "RGB control release exceeded the exit budget"),
        }
    }
}

fn choose_authoritative<T>(
    capture: Option<T>,
    capture_is_current: impl FnOnce(&T) -> bool,
    registry_lookup: impl FnOnce() -> Option<T>,
) -> Option<T> {
    match capture {
        Some(capture) if capture_is_current(&capture) => Some(capture),
        _ => registry_lookup(),
    }
}

/// The handles every device read and write goes through: the capture
/// session's channel slot, inventory's channel registry, the receiver lease,
/// the host device-I/O gate, and the per-transport request turns. Cheap to
/// clone; bind it to a device with [`Self::op`].
#[derive(Clone)]
pub struct DeviceAccess {
    /// The capture session's open channel, preferred for as long as inventory
    /// still publishes it.
    pub channel: CaptureChannelSlot,
    /// Exact-route channels owned and published by the inventory enumerator.
    pub registry: ChannelRegistry,
    /// Receiver access shared with HID++ sessions and pairing.
    pub receiver_access: ReceiverAccess,
    /// Host-lifecycle gate shared by every producer of proactive device I/O.
    pub device_io: DeviceIoGate,
    /// Shared FIFO turns so one physical transport runs one agent-owned HID
    /// request at a time, and the response timeout starts after the wait.
    pub(crate) transport_turns: TransportTurns,
}

impl DeviceAccess {
    /// Bind a device operation to `route`.
    #[must_use]
    pub fn op(&self, route: &DeviceRoute) -> DeviceOp {
        DeviceOp {
            access: self.clone(),
            route: route.clone(),
        }
    }
}

/// One device's HID++ write or read, bound to the agent's capture and
/// inventory channels for `route`. Built via [`DeviceAccess::op`], usually
/// through [`crate::orchestrator::SharedHandles::device`] or
/// [`crate::orchestrator::SharedHandles::keyboard_device`] — the receiver-side
/// counterpart of `openlogi_hid::write::with_route`'s "boilerplate-eater"
/// pattern, applied to an already-open channel instead of a fresh one.
pub struct DeviceOp {
    access: DeviceAccess,
    route: DeviceRoute,
}

/// Why a device operation has no channel to write through right now.
#[derive(Debug, Clone, Copy)]
enum Unresolved {
    /// The host gate refuses proactive device I/O (sleep, lock, shutdown).
    IoSuspended,
    /// Inventory publishes no channel for the route.
    NoChannel,
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::IoSuspended => "host device I/O suspended",
            Self::NoChannel => "no inventory channel",
        })
    }
}

/// A requester sees both as "nothing to write to".
impl From<Unresolved> for WriteError {
    fn from(_: Unresolved) -> Self {
        Self::DeviceNotFound
    }
}

impl DeviceOp {
    /// Resolve the authoritative channel without acquiring the receiver
    /// lease. Callers that manage their own lease/thread lifecycle across
    /// more than one write (the volatile-settings reapply sequence) resolve
    /// once up front through this instead of [`Self::run`]/[`Self::detach`].
    fn resolve(&self) -> Result<SharedChannel, Unresolved> {
        if !self.access.device_io.allows_io() {
            return Err(Unresolved::IoSuspended);
        }
        authoritative_channel(
            Some(&self.access.channel),
            &self.access.registry,
            &self.route,
        )
        .map_err(|_| Unresolved::NoChannel)
    }

    /// Lease the receiver, wait for this transport's HID turn, resolve the
    /// authoritative channel, then run `f` against it under `WRITE_TIMEOUT`,
    /// mapping a timeout to [`WriteError::RequestTimedOut`].
    ///
    /// Lease-then-turn-then-resolve, not the other way around: both waits are
    /// unbounded, and a channel resolved before them could be retired by the
    /// inventory enumerator while still queued — the write itself would likely
    /// still succeed on the stale handle, but anything that caches a feature
    /// off it (see the haptic feature cache's `EpochGuarded` note) would then
    /// pin a channel the enumerator can never reopen. The response budget
    /// starts only once the turn is held, so a reply-confirmed SmartShift or
    /// DPI write queued behind another slot on the same receiver still has the
    /// whole budget to send its request and check the device's reply. Used by
    /// the IPC server's ordinary reads and writes and the Actions Ring haptic
    /// path. Lighting uses [`Self::lighting`] so rollback outlives the
    /// requester's deadline.
    pub async fn run<F, Fut, T>(self, op: HidppOperation, f: F) -> Result<T, WriteError>
    where
        F: FnOnce(SharedChannel) -> Fut,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        if !self.access.device_io.allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        let turn = self.access.transport_turns.turn_for(&self.route);
        let _lease = self.access.receiver_access.acquire_for_io().await;
        after_transport_turn(&turn, async {
            let shared = self.resolve()?;
            timed(op, f(shared)).await
        })
        .await
    }

    /// [`Self::run`] for a write ordered by `ticket`: lease the receiver,
    /// wait for the device's earlier writes of the same setting, then resolve
    /// the channel and give `write` — or `superseded`, when a newer write was
    /// requested meanwhile — the whole `WRITE_TIMEOUT`.
    ///
    /// The wait is not part of the deadline: a pick queued behind a reconnect
    /// write to a mouse that stopped answering still gets its full budget once
    /// that write gives up. Resolving after the wait keeps [`Self::run`]'s
    /// rule of never queueing on an already-resolved channel.
    pub(crate) async fn run_in_turn<W, WriteFut, S, SupersededFut, T>(
        self,
        op: HidppOperation,
        ticket: &WriteTicket,
        write: W,
        superseded: S,
    ) -> Result<T, WriteError>
    where
        W: FnOnce(SharedChannel) -> WriteFut,
        WriteFut: Future<Output = Result<T, WriteError>>,
        S: FnOnce(SharedChannel) -> SupersededFut,
        SupersededFut: Future<Output = Result<T, WriteError>>,
    {
        if !self.access.device_io.allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        let turn = self.access.transport_turns.turn_for(&self.route);
        let _lease = self.access.receiver_access.acquire_for_io().await;
        within_turn(
            ticket,
            &turn,
            || async { write(self.resolve()?).await },
            || async { superseded(self.resolve()?).await },
        )
        .await
        .map_err(|_| WriteError::RequestTimedOut { operation: op })?
    }

    /// [`Self::run`] for a write that must land after every earlier write of
    /// the same setting and must not be skipped (see
    /// [`WriteTicket::in_order`]): wait for those writes, then lease the
    /// receiver, resolve the channel and give `write` the whole
    /// `WRITE_TIMEOUT`.
    ///
    /// The wait comes before the lease. The lease is a fair `RwLock`, so
    /// waiting while holding it could deadlock: a queued pairing session
    /// would block the earlier write's lease, which waits for this one.
    pub(crate) async fn run_in_order<W, WriteFut, T>(
        self,
        op: HidppOperation,
        ticket: WriteTicket,
        write: W,
    ) -> Result<T, WriteError>
    where
        W: FnOnce(SharedChannel) -> WriteFut,
        WriteFut: Future<Output = Result<T, WriteError>>,
    {
        if !self.access.device_io.allows_io() {
            return Err(WriteError::DeviceNotFound);
        }
        let turn = self.access.transport_turns.turn_for(&self.route);
        ticket
            .in_order(|| async {
                let _lease = self.access.receiver_access.acquire_for_io().await;
                after_transport_turn(&turn, async {
                    let shared = self.resolve()?;
                    timed(op, write(shared)).await
                })
                .await
            })
            .await
    }

    /// Own the whole lighting transaction outside the requester runtime. The
    /// worker leases and resolves AFTER obtaining its lighting route lock and
    /// retains that lease through any RGB rollback after requester cancellation.
    /// A write that leaves `0x8071` software control held records its lease,
    /// bound to the connection it went through, which
    /// [`crate::orchestrator::SharedHandles::release_rgb_control`] hands back
    /// when the agent leaves. Once that release has started, a new write is
    /// refused as [`WriteError::DeviceNotFound`].
    pub fn lighting(
        self,
        lighting: &Lighting,
    ) -> Result<openlogi_hid::lighting::LightingJob, WriteError> {
        let ticket = rgb_control::SESSION
            .admit()
            .ok_or(WriteError::DeviceNotFound)?;
        let DeviceAccess {
            channel: capture,
            registry,
            receiver_access,
            device_io,
            ..
        } = self.access;
        let route = self.route.clone();
        let (r, g, b) = lighting_rgb(lighting);
        let write = openlogi_hid::write::LightingWrite {
            method: openlogi_hid::LightingMethod::Auto,
            color: openlogi_core::color::Rgb::new(r, g, b),
        };
        openlogi_hid::lighting::LightingJob::spawn(&self.route, move |cancel| async move {
            let _lease = tokio::time::timeout(WRITE_TIMEOUT, receiver_access.acquire_for_io())
                .await
                .map_err(|_| WriteError::RequestTimedOut {
                    operation: HidppOperation::Lighting,
                })?;
            if !device_io.allows_io() {
                return Err(WriteError::DeviceNotFound);
            }
            let channel = authoritative_channel(Some(&capture), &registry, &route)?;
            let lease = write
                .apply_on(
                    &channel,
                    || cancel.is_cancelled(),
                    || device_io.allows_io() && registry.is_current(&channel),
                )
                .await?;
            if let Some(lease) = lease {
                ticket.record(&route, channel.downgrade(), lease);
            }
            Ok(())
        })
    }

    /// Return `lease` to the firmware through `connection`, the one the claim
    /// went through, under the same route lock, receiver lease and I/O gate as
    /// [`Self::lighting`]. A connection that has closed or stopped being the
    /// route's publication means a replug or another device: the lease
    /// describes nothing there, so nothing is sent. Behind an unchanged
    /// connection, such as a receiver slot paired to another device since,
    /// the release checks the unit ID its claim recorded.
    fn release_rgb_control(
        self,
        connection: WeakSharedChannel,
        lease: openlogi_hid::write::RgbControlLease,
    ) -> Result<openlogi_hid::lighting::LightingJob, WriteError> {
        let DeviceAccess {
            registry,
            receiver_access,
            device_io,
            ..
        } = self.access;
        openlogi_hid::lighting::LightingJob::spawn(&self.route, move |_| async move {
            let _lease = tokio::time::timeout(WRITE_TIMEOUT, receiver_access.acquire_for_io())
                .await
                .map_err(|_| WriteError::RequestTimedOut {
                    operation: HidppOperation::Lighting,
                })?;
            if !device_io.allows_io() {
                return Err(WriteError::DeviceNotFound);
            }
            let channel = connection
                .upgrade()
                .filter(|channel| registry.is_current(channel))
                .ok_or(WriteError::DeviceNotFound)?;
            openlogi_hid::write::release_rgb_control(&channel, lease, || {
                device_io.allows_io() && registry.is_current(&channel)
            })
            .await
        })
    }

    /// Fire-and-forget `f` on its own OS thread and one-shot runtime, with the
    /// standard three-arm outcome logging: a completed write and a failed
    /// write both log at their own level, keyed by `label`; a device that
    /// never answers within `WRITE_TIMEOUT` warns instead of hanging the
    /// thread forever.
    ///
    /// A registry miss is checked on the calling thread before spawning, so a
    /// write that already has no target never pays for a thread. The channel
    /// that check returns is dropped immediately. The write resolves the
    /// current channel again only after it holds the transport turn, so a
    /// connection retired while it is queued is not pinned and not written.
    pub fn detach<F, Fut, T>(self, label: &'static str, f: F)
    where
        F: FnOnce(SharedChannel) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        let index = self.route.device_index();
        self.spawn_write(label, f, move |result| {
            log_outcome(index, label, result, |_| {
                debug!(index, label, "background write completed");
            });
        });
    }

    /// Core of [`Self::detach`], with the outcome handed to `log` instead of
    /// an assumed logging shape. Used directly (bypassing `detach`) by any
    /// write whose input carries a value worth logging — a written DPI, a
    /// SmartShift config, an `on`/`off` flag, an RGB triple — so that value
    /// stays in the log line instead of collapsing to the generic
    /// `label`-only outcome message; native wheel-mode writes use the same
    /// seam to log a `FeatureUnsupported` result at `debug` (unsupported
    /// HiRes wheel/inversion is expected on plenty of mice), unlike every
    /// other background write's `warn`.
    fn spawn_write<F, Fut, T>(
        self,
        label: &'static str,
        f: F,
        log: impl FnOnce(Result<Result<T, WriteError>, Elapsed>) + Send + 'static,
    ) where
        F: FnOnce(SharedChannel) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, WriteError>>,
    {
        let turn = self.access.transport_turns.turn_for(&self.route);
        self.spawn_timed_write(
            label,
            move |op| {
                let turn = Arc::clone(&turn);
                async move { timed_on_current_channel(&op, &turn, f).await }
            },
            log,
        );
    }

    /// [`Self::spawn_write`] for a write that bounds its own device I/O, as
    /// `tokio::time::timeout` would: one that first waits its turn behind
    /// the device's earlier writes (see [`within_turn`]) must not spend its
    /// deadline on that wait. `f` resolves the channel itself, after that
    /// wait, and must not close over one resolved beforehand.
    fn spawn_timed_write<F, Fut, T>(
        self,
        label: &'static str,
        f: F,
        log: impl FnOnce(Result<Result<T, WriteError>, Elapsed>) + Send + 'static,
    ) where
        F: FnOnce(DeviceOp) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Result<T, WriteError>, Elapsed>>,
    {
        match self.resolve() {
            Ok(shared) => drop(shared),
            Err(reason) => {
                debug!(route = %self.route, label, %reason, "background write skipped");
                return;
            }
        }
        std::thread::spawn(move || {
            let Some(rt) = one_shot_runtime(label) else {
                return;
            };
            let result = rt.block_on(async move {
                let _lease = self.access.receiver_access.acquire_for_io().await;
                if !self.access.device_io.allows_io() {
                    return None;
                }
                Some(f(self).await)
            });
            if let Some(result) = result {
                log(result);
            } else {
                debug!(
                    label,
                    "host device I/O suspended — background write skipped"
                );
            }
        });
    }
}

/// Log how one bounded background write ended. Only the success line differs
/// between writes, because it carries the value written, so `written` logs
/// it. A failed write and one that timed out (an asleep or unresponsive
/// device) both warn, naming `what` and the device.
fn log_outcome<T>(
    index: u8,
    what: &str,
    result: Result<Result<T, WriteError>, Elapsed>,
    written: impl FnOnce(T),
) {
    match result {
        Ok(Ok(value)) => written(value),
        Ok(Err(error)) => warn!(index, ?error, "{what} failed"),
        Err(_) => warn!(index, "{what} timed out (device asleep/unresponsive)"),
    }
}

/// Wait for `ticket`'s turn, then for `transport`, then give whichever of
/// `write` and `superseded` runs the whole `WRITE_TIMEOUT`. Every ordered
/// write goes through here: neither wait is part of the deadline, and the
/// transport turn is held only while that bounded I/O runs.
async fn within_turn<T, W, WriteFut, S, SupersededFut>(
    ticket: &WriteTicket,
    transport: &TransportTurn,
    write: W,
    superseded: S,
) -> Result<T, Elapsed>
where
    W: FnOnce() -> WriteFut,
    WriteFut: Future<Output = T>,
    S: FnOnce() -> SupersededFut,
    SupersededFut: Future<Output = T>,
{
    ticket
        .write_or(
            || async {
                after_transport_turn(transport, async {
                    tokio::time::timeout(WRITE_TIMEOUT, write()).await
                })
                .await
            },
            || async {
                after_transport_turn(transport, async {
                    tokio::time::timeout(WRITE_TIMEOUT, superseded()).await
                })
                .await
            },
        )
        .await
}

/// Build the one-shot current-thread runtime every background write spawns
/// its OS thread onto. Logs and returns `None` on the rare case that
/// initialization itself fails (e.g. OS resource exhaustion).
fn one_shot_runtime(label: &str) -> Option<tokio::runtime::Runtime> {
    match openlogi_core::worker::runtime() {
        Ok(rt) => Some(rt),
        Err(e) => {
            warn!(error = %e, label, "tokio runtime init failed; write skipped");
            None
        }
    }
}

/// Spawn an OS thread that toggles SmartShift (free ↔ ratchet) on `op`'s
/// device via its current shared channel. Returns immediately; failures
/// (incl. devices that expose neither `0x2111` nor the older `0x2110`
/// SmartShift feature) are logged.
pub fn toggle_smartshift_in_background(op: DeviceOp) {
    let index = op.route.device_index();
    op.spawn_write(
        "SmartShift toggle",
        |c| async move { openlogi_hid::toggle_smartshift_on(&c).await },
        move |result| {
            log_outcome(index, "SmartShift toggle", result, |mode| {
                debug!(index, ?mode, "SmartShift toggled");
            });
        },
    );
}

/// Spawn an OS thread that writes the keyboard Fn-lock state to `op`'s device
/// via [`openlogi_hid::set_fn_lock_on`], unless a newer Fn-lock write for the
/// keyboard was requested after `ticket`. Returns immediately; failures (incl.
/// keyboards that expose neither `0x40a3` nor `0x40a2` fn inversion, and a
/// keyboard whose read-back disagrees with the write) are logged.
pub(crate) fn write_fn_lock_in_background(op: DeviceOp, ticket: WriteTicket, on: bool) {
    let index = op.route.device_index();
    let turn = op.access.transport_turns.turn_for(&op.route);
    op.spawn_timed_write(
        "Fn-lock write",
        move |op| async move {
            within_turn(
                &ticket,
                &turn,
                || async {
                    let shared = op.resolve()?;
                    openlogi_hid::set_fn_lock_on(&shared, on).await.map(Some)
                },
                || async { Ok(None) },
            )
            .await
        },
        move |result| {
            log_outcome(index, "Fn-lock write", result, |state| {
                if let Some(state) = state {
                    debug!(
                        index,
                        on,
                        default = state.default_fn_lock,
                        "Fn-lock written"
                    );
                } else {
                    debug!(index, on, "Fn-lock write superseded by a newer one");
                }
            });
        },
    );
}

/// The devices, by config key, whose "keep OS layout" is in effect as of the
/// latest config. The orchestrator adds a device when it queues a layout
/// write and drops it when a reload turns the setting off or disables the
/// device. A queued write can wait behind pairing or a host switch, so it
/// asks this set when it starts and again just before it writes, and does
/// not land after the user opted out.
#[derive(Debug, Default)]
pub(crate) struct KeptOsLayouts(Mutex<HashSet<String>>);

impl KeptOsLayouts {
    /// Record that `key`'s layout is kept.
    pub(crate) fn keep(&self, key: &str) {
        self.lock().insert(key.to_owned());
    }

    /// Drop every device for which `still_kept` says no.
    pub(crate) fn retain(&self, still_kept: impl Fn(&str) -> bool) {
        self.lock().retain(|key| still_kept(key));
    }

    /// Whether `key`'s layout is kept.
    pub(crate) fn contains(&self, key: &str) -> bool {
        self.lock().contains(key)
    }

    fn lock(&self) -> MutexGuard<'_, HashSet<String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Spawn an OS thread that keeps `op`'s keyboard on `host`'s `0x4531`
/// platform via [`openlogi_hid::keep_os_layout_on`], which writes only when
/// the current Easy-Switch slot holds another platform. The write is
/// withdrawn once `kept` no longer holds `key`. Returns immediately; failures
/// (incl. a keyboard whose read-back disagrees with the write) are logged.
pub(crate) fn keep_os_layout_in_background(
    op: DeviceOp,
    host: HostOs,
    kept: Arc<KeptOsLayouts>,
    key: String,
) {
    let index = op.route.device_index();
    op.spawn_write(
        "OS layout write",
        move |c| async move {
            openlogi_hid::keep_os_layout_on(&c, host, || kept.contains(&key)).await
        },
        move |result| {
            log_outcome(index, "OS layout write", result, |outcome| {
                if let OsLayoutOutcome::Switched { from, to } = outcome {
                    info!(index, ?from, to, "keyboard layout switched to the host OS");
                } else {
                    debug!(index, ?outcome, "keyboard layout left as it is");
                }
            });
        },
    );
}

/// The settings a mouse forgets when it power-cycles, as one reapply pushes
/// them. `None` leaves that setting alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VolatileMouseSettings {
    /// Native HiResWheel resolution and inversion.
    pub wheel: Option<WheelModeChange>,
    /// Sensor DPI.
    pub dpi: Option<Dpi>,
    /// Report rate. Written only when the device lists it, and skipped when
    /// the device already reports at it.
    pub report_rate: Option<ReportRate>,
    /// SmartShift mode and thresholds.
    pub smartshift: Option<SmartShiftStatus>,
}

impl VolatileMouseSettings {
    /// Whether there is nothing to push.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Re-apply every volatile mouse setting for `op`'s device on a **single**
/// background thread, sequentially, on the current inventory-owned channel.
///
/// Agent-start reapply used to fire DPI / SmartShift / wheel-mode each on its
/// own thread, and each opened a fresh HID++ channel when capture was not yet
/// ready. Concurrent opens of the same Bolt/Unifying node share the OS input
/// stream while correlating responses only by software id — they cross-talk and
/// produce the intermittent SmartShift `InvalidArgument` seen in #485. One
/// sequential writer removes that self-race, so this deliberately does NOT
/// decompose into three [`DeviceOp::detach`] calls: the lease is held for the
/// whole sequence, and every write below runs on the one OS thread spawned
/// here. Each write resolves the current channel only after it holds the
/// transport turn, so a connection retired while the sequence is queued is
/// not pinned. Takes `op` by reference (unlike every other function here)
/// because it only ever reads its fields — it never hands the operation
/// itself to [`DeviceOp::run`] or [`DeviceOp::detach`].
///
/// The report rate is the one setting here the GUI also writes on its own, so
/// its write is numbered in `report_rate_order` now, as the reapply is
/// scheduled: a pick the user makes before the reapply reaches the mouse is
/// newer, and the reapply then leaves the rate to it rather than writing the
/// configured one over it.
pub(crate) fn reapply_mouse_volatile_in_background(
    op: &DeviceOp,
    settings: VolatileMouseSettings,
    report_rate_order: &WriteOrder,
    capture_plans: &SharedCapturePlans,
) {
    match op.resolve() {
        Ok(shared) => drop(shared),
        Err(reason) => {
            debug!(route = %op.route, %reason, "volatile reapply skipped");
            return;
        }
    }
    let VolatileMouseSettings {
        wheel,
        dpi,
        report_rate,
        smartshift,
    } = settings;
    // Numbered only once the reapply will run: a ticket that never writes
    // would still supersede a pick already waiting for its turn.
    let report_rate = report_rate.map(|rate| (rate, report_rate_order.request(&op.route)));
    let access = op.access.clone();
    let route = op.route.clone();
    let index = route.device_index();
    let capture_plans = capture_plans.clone();
    std::thread::spawn(move || {
        let Some(rt) = one_shot_runtime("volatile reapply") else {
            return;
        };
        rt.block_on(async move {
            let _lease = access.receiver_access.acquire_for_io().await;
            if !access.device_io.allows_io() {
                debug!(
                    index,
                    "host device I/O suspended — volatile reapply skipped"
                );
                return;
            }
            let op = DeviceOp { access, route };
            let turn = op.access.transport_turns.turn_for(&op.route);
            if let Some(change) = wheel {
                let (plans, route) = (&capture_plans, &op.route);
                let result = timed_on_current_channel(&op, &turn, |shared| async move {
                    write_native_wheel_mode(change, &shared, plans, route).await
                })
                .await;
                log_wheel_result(index, change, result);
            }
            if let Some(dpi) = dpi {
                let result = timed_on_current_channel(&op, &turn, |shared| async move {
                    openlogi_hid::set_dpi_on(&shared, dpi).await
                })
                .await;
                log_outcome(index, "DPI write", result, |()| {
                    debug!(index, %dpi, "DPI written to device");
                });
            }
            if let Some((rate, ticket)) = report_rate {
                let result = within_turn(
                    &ticket,
                    &turn,
                    || async {
                        let shared = op.resolve()?;
                        openlogi_hid::set_report_rate_on(&shared, rate)
                            .await
                            .map(Some)
                    },
                    || async { Ok(None) },
                )
                .await;
                log_outcome(index, "report-rate write", result, |info| {
                    if let Some(info) = info {
                        debug!(index, rate = %info.current, "report rate written to device");
                    } else {
                        debug!(index, %rate, "report-rate reapply superseded by a newer write");
                    }
                });
            }
            if let Some(ss) = smartshift {
                let result = timed_on_current_channel(&op, &turn, |shared| async move {
                    openlogi_hid::set_smartshift_on(&shared, ss).await
                })
                .await;
                log_outcome(index, "SmartShift write", result, |()| {
                    debug!(index, status = ?ss, "SmartShift config written");
                });
            }
        });
    });
}

/// A change to a device's native HiResWheel mode. What a variant leaves out
/// keeps the device's current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelModeChange {
    /// Set the resolution and preserve the inversion bit.
    Resolution(ScrollResolution),
    /// Set the inversion bit and preserve the resolution.
    Inversion(bool),
    /// Set both.
    ResolutionAndInversion {
        /// The resolution to set.
        resolution: ScrollResolution,
        /// The inversion bit to set.
        inverted: bool,
    },
}

impl WheelModeChange {
    /// The change that sets whichever of the two is configured, or `None`
    /// when neither is and the wheel stays unmanaged.
    #[must_use]
    pub fn new(resolution: Option<ScrollResolution>, inverted: Option<bool>) -> Option<Self> {
        match (resolution, inverted) {
            (Some(resolution), Some(inverted)) => Some(Self::ResolutionAndInversion {
                resolution,
                inverted,
            }),
            (Some(resolution), None) => Some(Self::Resolution(resolution)),
            (None, Some(inverted)) => Some(Self::Inversion(inverted)),
            (None, None) => None,
        }
    }

    /// Write the change through `shared`.
    ///
    /// # Errors
    ///
    /// Whatever the HiResWheel write reports, including
    /// [`WriteError::FeatureUnsupported`] on a wheel without the feature.
    pub async fn apply_on(self, shared: &SharedChannel) -> Result<(), WriteError> {
        match self {
            Self::ResolutionAndInversion {
                resolution,
                inverted,
            } => openlogi_hid::set_scroll_wheel_mode_on(shared, resolution, inverted)
                .await
                .map(|_| ()),
            Self::Resolution(resolution) => {
                openlogi_hid::set_scroll_resolution_on(shared, resolution)
                    .await
                    .map(|_| ())
            }
            Self::Inversion(inverted) => {
                openlogi_hid::set_scroll_inversion_on(shared, inverted).await
            }
        }
    }
}

fn log_wheel_result(
    index: u8,
    change: WheelModeChange,
    result: Result<Result<NativeWheelWrite, WriteError>, Elapsed>,
) {
    if let Ok(Ok(NativeWheelWrite::StoodDown)) = &result {
        debug!(
            index,
            ?change,
            "native wheel mode not written: a capture session owns the wheel"
        );
        return;
    }
    // Plenty of mice have no HiResWheel or no inversion bit, so that refusal
    // is expected and stays out of the warnings every other write earns.
    if let Ok(Err(WriteError::FeatureUnsupported { feature_hex })) = &result {
        debug!(
            index,
            ?change,
            feature = format_args!("{feature_hex:#06x}"),
            "native wheel mode unsupported"
        );
        return;
    }
    log_outcome(index, "wheel mode write", result, |_written| {
        debug!(index, ?change, "native wheel mode written");
    });
}

/// Spawn an OS thread that writes `dpi` to `op`'s device via its current
/// shared channel. Returns immediately; failures are logged.
pub fn write_dpi_in_background(op: DeviceOp, dpi: Dpi) {
    let index = op.route.device_index();
    op.spawn_write(
        "DPI write",
        move |c| async move { openlogi_hid::set_dpi_on(&c, dpi).await },
        move |result| {
            log_outcome(index, "DPI write", result, |()| {
                debug!(index, %dpi, "DPI written to device");
            });
        },
    );
}

/// Spawn an OS thread that reconciles the configured native HiResWheel mode
/// for `op`'s device. Unsupported devices are expected and only logged at
/// debug level. The write stands down if, by its turn, `capture_plans` hands
/// the wheel to a capture session (see `write_native_wheel_mode`).
pub fn write_scroll_wheel_mode_in_background(
    op: DeviceOp,
    change: WheelModeChange,
    capture_plans: SharedCapturePlans,
) {
    let index = op.route.device_index();
    let route = op.route.clone();
    op.spawn_write(
        "wheel mode write",
        move |shared| async move {
            write_native_wheel_mode(change, &shared, &capture_plans, &route).await
        },
        move |result| log_wheel_result(index, change, result),
    );
}

/// What a native wheel-mode write did once it held its transport turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeWheelWrite {
    /// The change was written.
    Written,
    /// A capture session now owns the wheel, so nothing was written.
    StoodDown,
}

/// Write `change` through `shared` unless the latest capture publication
/// hands the main wheel at `route` to a capture session.
///
/// Every resolution or inversion write also selects native reporting, so a
/// write queued before a plan diverted the wheel would hand it back to the OS
/// and leave the session's eased scrolling silently off until it re-arms. The
/// orchestrator queues no such write while the wheel is diverted; this check,
/// made once the write holds its turn, retires the ones queued before.
async fn write_native_wheel_mode(
    change: WheelModeChange,
    shared: &SharedChannel,
    capture_plans: &SharedCapturePlans,
    route: &DeviceRoute,
) -> Result<NativeWheelWrite, WriteError> {
    if capture_plans.borrow().diverts_main_wheel(route) {
        return Ok(NativeWheelWrite::StoodDown);
    }
    change.apply_on(shared).await?;
    if capture_plans.borrow().diverts_main_wheel(route) {
        // The plan diverted the wheel while this write was on the wire, so it
        // may have landed after the session's divert.
        warn!(
            %route,
            ?change,
            "native wheel mode written as capture took the wheel; smooth \
             scrolling may stay off until the session re-arms"
        );
    }
    Ok(NativeWheelWrite::Written)
}

/// Apply `lighting` to the keyboard at `op`'s device on a background thread.
///
/// Resolves the configured colour (scaled by brightness, or black when the
/// lighting is off) and writes every key over HID++ via
/// [`openlogi_hid::set_keyboard_color_on`]. A registry miss and write
/// failures are logged, not surfaced.
pub fn set_lighting_in_background(op: DeviceOp, lighting: &Lighting) {
    match op.lighting(lighting) {
        Ok(job) => job.detach(),
        Err(error) => warn!(?error, "could not start background lighting"),
    }
}

/// Resolve a [`Lighting`] config to an `(r, g, b)` triple: the configured
/// colour scaled by brightness, or black when lighting is off.
#[must_use]
pub fn lighting_rgb(lighting: &Lighting) -> (u8, u8, u8) {
    if !lighting.enabled {
        return (0, 0, 0);
    }
    let (r, g, b) = lighting.color.components();
    let scale =
        |c: u8| u8::try_from(u16::from(c) * u16::from(lighting.brightness) / 100).unwrap_or(c);
    (scale(r), scale(g), scale(b))
}

/// Bound any single HID++ call by [`WRITE_TIMEOUT`] so an asleep / unresponsive
/// device can't hang the awaiting IPC handler indefinitely.
async fn timed<T>(
    operation: HidppOperation,
    fut: impl Future<Output = Result<T, WriteError>>,
) -> Result<T, WriteError> {
    tokio::time::timeout(WRITE_TIMEOUT, fut)
        .await
        .map_err(|_| WriteError::RequestTimedOut { operation })?
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::capture_plan::CapturePublication;
    use openlogi_hid::device_io_channel;

    fn device_op(
        capture: &CaptureChannelSlot,
        registry: &ChannelRegistry,
        receiver_access: &ReceiverAccess,
        device_io: &DeviceIoGate,
        route: &DeviceRoute,
    ) -> DeviceOp {
        DeviceAccess {
            channel: capture.clone(),
            registry: registry.clone(),
            receiver_access: receiver_access.clone(),
            device_io: device_io.clone(),
            transport_turns: TransportTurns::default(),
        }
        .op(route)
    }

    #[test]
    fn current_capture_wins_without_consulting_the_registry_again() {
        let looked_up = Cell::new(false);
        let selected = choose_authoritative(
            Some("capture"),
            |_| true,
            || {
                looked_up.set(true);
                Some("registry")
            },
        );

        assert_eq!(selected, Some("capture"));
        assert!(!looked_up.get());
    }

    #[test]
    fn stale_capture_falls_through_to_the_registry_winner() {
        let selected = choose_authoritative(Some("stale"), |_| false, || Some("registry-current"));

        assert_eq!(selected, Some("registry-current"));
    }

    #[test]
    fn registry_miss_has_no_route_open_fallback() {
        let selected = choose_authoritative(Some("stale"), |_| false, || None);

        assert_eq!(selected, None);
    }

    fn unresolvable_route() -> DeviceRoute {
        // A route no capture/registry in this test ever publishes — every
        // resolve attempt against it takes the registry-miss path.
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc52b,
        }
    }

    /// `DeviceOp::run` must fail fast on a registry miss ([`DeviceNotFound`])
    /// and must never invoke `f` — a route that can't be resolved has no
    /// channel to hand it, so running the caller's write would be a bug, not a
    /// no-op.
    #[tokio::test]
    async fn run_on_a_registry_miss_returns_device_not_found_without_calling_f() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (_device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);

        let result = device_op(&capture, &registry, &receiver_access, &device_io, &route)
            .run(HidppOperation::WriteDpi, move |_shared| {
                called_for_closure.store(true, Ordering::SeqCst);
                async move { Ok::<(), WriteError>(()) }
            })
            .await;

        assert!(matches!(result, Err(WriteError::DeviceNotFound)));
        assert!(
            !called.load(Ordering::SeqCst),
            "f must not run when the route can't be resolved"
        );
    }

    #[tokio::test]
    async fn run_while_device_io_is_suspended_does_not_wait_for_a_receiver_or_call_f() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);
        let _exclusive = receiver_access
            .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing)
            .await;
        assert!(device_io_signal.suspend());

        let result = tokio::time::timeout(
            Duration::from_millis(10),
            device_op(&capture, &registry, &receiver_access, &device_io, &route).run(
                HidppOperation::WriteDpi,
                move |_shared| {
                    called_for_closure.store(true, Ordering::SeqCst);
                    async move { Ok::<(), WriteError>(()) }
                },
            ),
        )
        .await
        .expect("a suspended operation must fail before waiting for receiver access");

        assert!(matches!(result, Err(WriteError::DeviceNotFound)));
        assert!(
            !called.load(Ordering::SeqCst),
            "the write closure must not run while host device I/O is suspended",
        );
    }

    /// Every combination of the two independently configured wheel settings.
    /// `Some(false)` is a configured inversion, not an absent one.
    #[test]
    fn a_wheel_mode_change_sets_exactly_what_is_configured() {
        let low = ScrollResolution::Low;

        assert_eq!(WheelModeChange::new(None, None), None);
        assert_eq!(
            WheelModeChange::new(Some(low), None),
            Some(WheelModeChange::Resolution(low))
        );
        assert_eq!(
            WheelModeChange::new(None, Some(false)),
            Some(WheelModeChange::Inversion(false))
        );
        assert_eq!(
            WheelModeChange::new(Some(low), Some(true)),
            Some(WheelModeChange::ResolutionAndInversion {
                resolution: low,
                inverted: true,
            })
        );
    }

    /// `DeviceOp::detach` resolves before spawning, so a registry miss must
    /// return synchronously (no thread, no lease wait) and never call `f`.
    #[tokio::test]
    async fn detach_on_a_registry_miss_never_calls_f() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (_device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let called = std::sync::Arc::new(AtomicBool::new(false));
        let called_for_closure = std::sync::Arc::clone(&called);

        device_op(&capture, &registry, &receiver_access, &device_io, &route).detach(
            "test write",
            move |_shared| {
                called_for_closure.store(true, Ordering::SeqCst);
                async move { Ok::<(), WriteError>(()) }
            },
        );

        assert!(
            !called.load(Ordering::SeqCst),
            "f must not run when the route can't be resolved"
        );
    }

    /// A reapply that cannot reach the mouse numbers no report-rate write:
    /// a ticket that never writes would still make a pick already waiting
    /// for its turn read the mouse instead of writing the user's rate.
    #[tokio::test]
    async fn a_reapply_that_cannot_resolve_leaves_a_waiting_pick_its_turn() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (_device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let rates = WriteOrder::default();
        let pick = rates.request(&route);

        reapply_mouse_volatile_in_background(
            &device_op(&capture, &registry, &receiver_access, &device_io, &route),
            VolatileMouseSettings {
                report_rate: Some(ReportRate::new(1000)),
                ..VolatileMouseSettings::default()
            },
            &rates,
            &no_capture_plans(),
        );

        assert!(
            pick.write_or(|| async { true }, || async { false }).await,
            "the pick is still the newest report-rate write"
        );
    }

    /// A backlight change to a keyboard that cannot be resolved fails, and
    /// its turn passes on: the change requested after it still runs.
    #[tokio::test]
    async fn an_in_order_write_that_cannot_resolve_passes_its_turn_on() {
        let capture: CaptureChannelSlot = std::sync::Arc::new(RwLock::new(None));
        let registry = ChannelRegistry::default();
        let receiver_access = ReceiverAccess::default();
        let (_device_io_signal, device_io) = device_io_channel();
        let route = unresolvable_route();
        let backlight = WriteOrder::default();
        let first = backlight.request(&route);
        let next = backlight.request(&route);
        let failed = device_op(&capture, &registry, &receiver_access, &device_io, &route)
            .run_in_order(HidppOperation::WriteBacklight, first, |_| async {
                Ok::<_, WriteError>(())
            })
            .await;
        assert!(matches!(failed, Err(WriteError::DeviceNotFound)));
        let ran =
            tokio::time::timeout(Duration::from_secs(1), next.in_order(|| async { true })).await;
        assert_eq!(ran.ok(), Some(true), "the next change gets its turn");
    }

    /// A reconnect write to a mouse that stopped answering holds the turn
    /// until its deadline. The pick queued behind it still gets the whole
    /// deadline once its turn comes, rather than what the wait left over.
    #[tokio::test(start_paused = true)]
    async fn a_write_queued_behind_a_stalled_one_keeps_its_whole_deadline() {
        let rates = WriteOrder::default();
        let route = unresolvable_route();
        let stalled = rates.request(&route);
        let (started, has_started) = tokio::sync::oneshot::channel();
        let transport = TransportTurns::default().turn_for(&route);
        let stalled_transport = Arc::clone(&transport);
        let reapply = tokio::spawn(async move {
            within_turn(
                &stalled,
                &stalled_transport,
                || async move {
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                },
                || async {},
            )
            .await
        });
        has_started.await.expect("the stalled write holds the turn");

        let pick = rates.request(&route);
        let picked = within_turn(
            &pick,
            &transport,
            || async {
                tokio::time::sleep(WRITE_TIMEOUT / 2).await;
                2000
            },
            || async { 0 },
        )
        .await;

        assert_eq!(picked.ok(), Some(2000));
        assert!(
            reapply.await.expect("reapply joins").is_err(),
            "the stalled write gave up at its own deadline"
        );
    }

    /// A reply-confirmed SmartShift or DPI write checks the device's echo inside
    /// the same response budget as the request. Slots on one receiver share a
    /// writer, so a confirmed write queued behind the other slot must not spend
    /// that budget while it waits, and must still have the whole budget — long
    /// enough to send and confirm — once the writer is free. Another transport
    /// is not queued behind that receiver.
    #[tokio::test(start_paused = true)]
    async fn queued_reply_confirmed_write_keeps_its_whole_response_budget() {
        let turns = TransportTurns::default();
        let blocking = turns.turn_for(&DeviceRoute::Bolt {
            receiver_uid: "receiver".into(),
            slot: 1,
        });
        let held = blocking.lock().await;

        let started = Arc::new(AtomicBool::new(false));
        let write_started = Arc::clone(&started);
        let queued_turns = turns.clone();
        let queued = tokio::spawn(async move {
            let turn = queued_turns.turn_for(&DeviceRoute::Bolt {
                receiver_uid: "receiver".into(),
                slot: 2,
            });
            after_transport_turn(&turn, async move {
                tokio::time::timeout(WRITE_TIMEOUT, async move {
                    write_started.store(true, Ordering::SeqCst);
                    tokio::time::sleep(
                        WRITE_TIMEOUT
                            .checked_sub(Duration::from_millis(1))
                            .expect("response budget is longer than one millisecond"),
                    )
                    .await;
                    Ok::<(), WriteError>(())
                })
                .await
            })
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(WRITE_TIMEOUT + Duration::from_millis(1)).await;
        assert!(
            !queued.is_finished(),
            "queue time must not consume the reply-confirmed response budget"
        );
        assert!(
            !started.load(Ordering::SeqCst),
            "the write and its echo check wait until the transport is free"
        );

        let direct = after_transport_turn(
            &turns.turn_for(&DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xb034,
            }),
            async { true },
        )
        .await;
        assert!(
            direct,
            "a different transport is not queued behind the receiver"
        );

        drop(held);
        for _ in 0..8 {
            if started.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            started.load(Ordering::SeqCst),
            "the confirmed write starts once its transport turn is held"
        );
        tokio::time::advance(
            WRITE_TIMEOUT
                .checked_sub(Duration::from_millis(1))
                .expect("response budget is longer than one millisecond"),
        )
        .await;

        queued
            .await
            .expect("confirmed write must not panic")
            .expect("queue wait must not spend the response budget")
            .expect("the echo check still fits in the budget that started after the queue");
    }

    /// A background worker queued on a transport must not keep the connection
    /// it observed at schedule time. Inventory reopens a node only after every
    /// other owner drops it, and the write has to use the channel published
    /// once the turn is actually held.
    #[tokio::test]
    async fn queued_background_write_resolves_the_channel_after_the_turn() {
        let published = publish_direct_mouse().await;
        let turns = TransportTurns::default();
        let access = DeviceAccess {
            channel: std::sync::Arc::new(RwLock::new(None)),
            registry: published.registry.clone(),
            receiver_access: ReceiverAccess::default(),
            device_io: published.device_io.clone(),
            transport_turns: turns.clone(),
        };
        let turn = turns.turn_for(&published.route);
        let held = turn.lock().await;
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        access.op(&published.route).spawn_write(
            "queued write",
            move |shared| {
                let _ = seen_tx.send(shared.downgrade());
                async move { Ok::<(), WriteError>(()) }
            },
            |_| {},
        );

        published.retire_original().await;
        assert!(
            published.retired.upgrade().is_none(),
            "a queued worker must not keep the retired connection alive"
        );
        assert!(
            published.registry.lookup(&published.route).is_none(),
            "inventory drops the retired publication before the replacement opens"
        );

        published.publish_replacement().await;
        let replacement = published
            .registry
            .lookup(&published.route)
            .expect("the replacement connection is current")
            .downgrade();
        assert!(
            published.retired.upgrade().is_none(),
            "opening the replacement must not resurrect the retired connection"
        );

        drop(held);
        let seen = tokio::time::timeout(Duration::from_secs(2), seen_rx)
            .await
            .expect("the queued write must run once the turn is free")
            .expect("the write records the channel it resolved");
        assert!(
            seen == replacement,
            "the write must use the channel resolved after the turn"
        );
        assert!(
            published.retired.upgrade().is_none(),
            "the finished write must not have revived the retired connection"
        );
    }

    /// Volatile reapply waits out the same transport queue. It has to drop the
    /// connection it checked at schedule time, or a reload queued behind
    /// another slot pins a node inventory is trying to reopen.
    #[tokio::test]
    async fn queued_volatile_reapply_drops_the_connection_it_checked() {
        let published = publish_direct_mouse().await;
        let turns = TransportTurns::default();
        let receiver_access = ReceiverAccess::default();
        let access = DeviceAccess {
            channel: std::sync::Arc::new(RwLock::new(None)),
            registry: published.registry.clone(),
            receiver_access: receiver_access.clone(),
            device_io: published.device_io.clone(),
            transport_turns: turns.clone(),
        };
        let turn = turns.turn_for(&published.route);
        let held = turn.lock().await;
        reapply_mouse_volatile_in_background(
            &access.op(&published.route),
            VolatileMouseSettings {
                dpi: Some(Dpi::new(800)),
                ..VolatileMouseSettings::default()
            },
            &WriteOrder::default(),
            &no_capture_plans(),
        );
        wait_until_a_worker_holds_the_receiver(&receiver_access).await;

        published.retire_original().await;
        assert!(
            published.retired.upgrade().is_none(),
            "reapply must not pin the retired connection while it waits for the transport"
        );

        drop(held);
        tokio::time::timeout(
            Duration::from_secs(2),
            receiver_access
                .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing),
        )
        .await
        .expect("reapply finishes without a connection instead of writing the retired one");
        assert!(published.retired.upgrade().is_none());
    }

    /// A capture publication with no plans: no session owns any wheel.
    fn no_capture_plans() -> SharedCapturePlans {
        tokio::sync::watch::channel(Arc::default()).1
    }

    /// The publication of one capture plan for `route`, which hands that
    /// device's main wheel to its session when `divert`.
    fn wheel_publication(route: &DeviceRoute, divert: bool) -> Arc<CapturePublication> {
        let mut plan = crate::capture_plan::plan_for_device(
            &openlogi_core::config::Config::default(),
            openlogi_core::device_order::PhysicalDeviceKey::parse("unit:00000001")
                .expect("fixture is a physical key"),
            "mouse",
            route.clone(),
            None,
            0,
            true,
        );
        plan.target.spec.divert_main_wheel =
            divert.then(openlogi_hid::session::gesture::MainWheelDivert::default);
        Arc::new(CapturePublication::from_plans(vec![plan]))
    }

    /// The two background writes that set a native wheel mode.
    #[derive(Debug, Clone, Copy)]
    enum WheelWritePath {
        /// The reload push of a changed resolution or inversion.
        Reload,
        /// The reconnect re-apply of every volatile setting.
        Reapply,
    }

    /// Reports the replay mouse receives from a wheel write queued on `path`
    /// behind a held transport turn, after the capture publication changed
    /// while it waited to one that diverts the wheel when `divert_while_queued`.
    async fn reports_from_a_queued_wheel_write(
        path: WheelWritePath,
        divert_while_queued: bool,
    ) -> usize {
        let published = publish_direct_mouse().await;
        let turns = TransportTurns::default();
        let receiver_access = ReceiverAccess::default();
        let access = DeviceAccess {
            channel: std::sync::Arc::new(RwLock::new(None)),
            registry: published.registry.clone(),
            receiver_access: receiver_access.clone(),
            device_io: published.device_io.clone(),
            transport_turns: turns.clone(),
        };
        let (publish, plans) =
            tokio::sync::watch::channel(wheel_publication(&published.route, false));
        let turn = turns.turn_for(&published.route);
        let held = turn.lock().await;
        let change = WheelModeChange::Resolution(ScrollResolution::Low);
        match path {
            WheelWritePath::Reload => {
                write_scroll_wheel_mode_in_background(
                    access.op(&published.route),
                    change,
                    plans.clone(),
                );
            }
            WheelWritePath::Reapply => reapply_mouse_volatile_in_background(
                &access.op(&published.route),
                VolatileMouseSettings {
                    wheel: Some(change),
                    ..VolatileMouseSettings::default()
                },
                &WriteOrder::default(),
                &plans,
            ),
        }
        wait_until_a_worker_holds_the_receiver(&receiver_access).await;
        publish.send_replace(wheel_publication(&published.route, divert_while_queued));
        let written = || {
            published
                .backend
                .channel_completion(ORIGINAL_CHANNEL)
                .expect("known channel")
                .written_reports
                .len()
        };
        let before = written();
        drop(held);
        tokio::time::timeout(
            Duration::from_secs(2),
            receiver_access
                .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing),
        )
        .await
        .expect("the queued write finishes");
        written() - before
    }

    /// A wheel write queued before a plan handed the wheel to a capture
    /// session stands down at its turn on either path: writing a native mode
    /// would select native reporting under the session. With the wheel still
    /// native, the same write reaches the mouse.
    #[tokio::test]
    async fn a_queued_wheel_write_stands_down_once_capture_owns_the_wheel() {
        for path in [WheelWritePath::Reload, WheelWritePath::Reapply] {
            assert_eq!(
                reports_from_a_queued_wheel_write(path, true).await,
                0,
                "{path:?}: the diverted wheel is left to its session"
            );
            assert!(
                reports_from_a_queued_wheel_write(path, false).await > 0,
                "{path:?}: a native wheel still gets the configured mode"
            );
        }
    }

    struct PublishedMouse {
        backend: std::sync::Arc<openlogi_hid::replay::ReplayBackend>,
        registry: ChannelRegistry,
        device_io: DeviceIoGate,
        route: DeviceRoute,
        original: openlogi_hid::NodeId,
        replacement_node: openlogi_hid::NodeId,
        retired: openlogi_hid::WeakSharedChannel,
    }

    impl PublishedMouse {
        async fn retire_original(&self) {
            self.backend
                .set_node_presence(&self.original, openlogi_hid::replay::NodePresence::Absent)
                .expect("known node");
            self.backend
                .set_channel_connection(
                    ORIGINAL_CHANNEL,
                    openlogi_hid::replay::ChannelConnection::Disconnected,
                )
                .expect("known channel");
            self.enumerate().await;
        }

        async fn publish_replacement(&self) {
            self.backend
                .set_node_presence(
                    &self.replacement_node,
                    openlogi_hid::replay::NodePresence::Present,
                )
                .expect("known node");
            self.enumerate().await;
        }

        async fn enumerate(&self) {
            openlogi_hid::Enumerator::with_backend(self.backend.clone())
                .with_registry(self.registry.clone())
                .enumerate()
                .await
                .expect("replay enumeration");
        }
    }

    const ORIGINAL_CHANNEL: &str = "retired-mouse";
    const REPLACEMENT_CHANNEL: &str = "replacement-mouse";

    async fn publish_direct_mouse() -> PublishedMouse {
        use openlogi_fixture::{FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport};
        use openlogi_hid::replay::{
            ChannelConnection, NodePresence, OpenOutcome, RawWriterAvailability, ReplayBackend,
            ReplayChannel, ReplayNode, ReplayTopology,
        };
        use openlogi_hid::{NodeId, NodeInfo};

        let route = DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb35b,
        };
        let original = NodeId::from("retired-mouse-node".to_string());
        let replacement_node = NodeId::from("replacement-mouse-node".to_string());
        let node = |id: NodeId, presence: NodePresence, channel: &str| ReplayNode {
            info: NodeInfo {
                id,
                vendor_id: 0x046d,
                product_id: 0xb35b,
                usage_page: 0xff00,
                usage_id: 0x0002,
                name: "Replay Mouse".to_string(),
                manufacturer: Some("Logitech".to_string()),
                serial_number: None,
            },
            presence,
            open_outcome: OpenOutcome::Hidpp,
            channel: Some(channel.to_string()),
            raw_writer: RawWriterAvailability::Unavailable,
            receiver_slots: Vec::new(),
        };
        let cassette = |channel: &str| HidCassette {
            schema_version: FIXTURE_SCHEMA_VERSION,
            name: channel.to_string(),
            channel: channel.to_string(),
            report_support: ReportSupport::ShortAndLong,
            exchanges: direct_probe_exchanges(),
        };
        let backend = std::sync::Arc::new(
            ReplayBackend::new(
                ReplayTopology {
                    nodes: vec![
                        node(original.clone(), NodePresence::Present, ORIGINAL_CHANNEL),
                        node(
                            replacement_node.clone(),
                            NodePresence::Absent,
                            REPLACEMENT_CHANNEL,
                        ),
                    ],
                    channels: vec![
                        ReplayChannel {
                            id: ORIGINAL_CHANNEL.to_string(),
                            connection: ChannelConnection::Connected,
                            report_support: ReportSupport::ShortAndLong,
                        },
                        ReplayChannel {
                            id: REPLACEMENT_CHANNEL.to_string(),
                            connection: ChannelConnection::Connected,
                            report_support: ReportSupport::ShortAndLong,
                        },
                    ],
                },
                vec![cassette(ORIGINAL_CHANNEL), cassette(REPLACEMENT_CHANNEL)],
            )
            .expect("valid replay topology"),
        );
        let (_device_io_signal, device_io) = device_io_channel();
        let registry = ChannelRegistry::default();
        openlogi_hid::Enumerator::with_backend(backend.clone())
            .with_registry(registry.clone())
            .enumerate()
            .await
            .expect("the original mouse publishes");
        let retired = registry
            .lookup(&route)
            .expect("inventory publishes the original connection")
            .downgrade();
        PublishedMouse {
            backend,
            registry,
            device_io,
            route,
            original,
            replacement_node,
            retired,
        }
    }

    fn direct_probe_exchanges() -> Vec<openlogi_fixture::CassetteExchange> {
        use openlogi_fixture::{CassetteExchange, RequestMatch};

        let h20 = |request: Vec<u8>, response: Vec<u8>| CassetteExchange {
            request_match: RequestMatch::Hidpp20,
            request,
            response: Some(response),
            required: true,
        };
        let short = |device: u8, feature: u8, function: u8, payload: [u8; 3]| {
            vec![
                0x10, device, feature, function, payload[0], payload[1], payload[2],
            ]
        };
        vec![
            h20(
                short(0xff, 0x00, 0x10, [0, 0, 0]),
                short(0xff, 0x00, 0x10, [4, 0, 0]),
            ),
            h20(
                short(0xff, 0x00, 0x00, [0x00, 0x01, 0]),
                short(0xff, 0x00, 0x00, [0x01, 0, 0]),
            ),
            h20(
                short(0xff, 0x01, 0x00, [0, 0, 0]),
                short(0xff, 0x01, 0x00, [2, 0, 0]),
            ),
            h20(
                short(0xff, 0x01, 0x10, [1, 0, 0]),
                short(0xff, 0x01, 0x10, [0x00, 0x01, 0]),
            ),
            h20(
                short(0xff, 0x01, 0x10, [2, 0, 0]),
                short(0xff, 0x01, 0x10, [0x22, 0x01, 0]),
            ),
        ]
    }

    async fn wait_until_a_worker_holds_the_receiver(access: &ReceiverAccess) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let attempt = tokio::time::timeout(
                    Duration::from_millis(20),
                    access
                        .acquire_exclusive(crate::receiver_access::ExclusiveAccessReason::Pairing),
                )
                .await;
                if attempt.is_err() {
                    break;
                }
                drop(attempt);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the reapply thread reaches the transport queue");
    }

    /// The timeout every [`DeviceOp::run`] call relies on: a write that never
    /// resolves within `WRITE_TIMEOUT` must map to
    /// [`WriteError::RequestTimedOut`] carrying the operation, not hang
    /// forever. Uses a paused clock so the test doesn't spend `WRITE_TIMEOUT`
    /// (5s) of real wall-clock time.
    #[tokio::test(start_paused = true)]
    async fn timed_maps_an_elapsed_deadline_to_request_timed_out() {
        let handle = tokio::spawn(timed(
            HidppOperation::WriteDpi,
            std::future::pending::<Result<(), WriteError>>(),
        ));
        // Let the spawned task run up to its first await point so the
        // underlying sleep is armed before we fast-forward the clock past it.
        tokio::task::yield_now().await;
        tokio::time::advance(WRITE_TIMEOUT + Duration::from_millis(1)).await;

        let result = handle.await.expect("timed task must not panic");

        assert!(matches!(
            result,
            Err(WriteError::RequestTimedOut {
                operation: HidppOperation::WriteDpi
            })
        ));
    }
}
