use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use hidpp::{
    channel::HidppChannel,
    receiver::{self, Receiver},
};
use openlogi_core::device::DeviceInventory;
use tokio::time::timeout;
use tracing::warn;

use super::events::EventSubscriptionHandle;
use crate::backend::NodeInfo;
use crate::channel::route::is_receiver_pid;
use crate::host_lock::{self, ReceiverRegisterPhase};

use super::cache::{CacheKey, CacheOutcome, Cached};

mod bolt;
mod direct;
mod unifying;

#[cfg(test)]
pub(super) use bolt::assemble_bolt_probe;
use bolt::probe_bolt_receiver;
#[cfg(test)]
pub(super) use direct::preferred_direct_codename;
use direct::probe_direct;
use unifying::probe_unifying_receiver;
#[cfg(test)]
pub(super) use unifying::{
    assemble_unifying_device, parse_codename, probe_unifying_slot, retry_arrival_trigger,
    unifying_probe_budget,
};

/// How long a device-arrival stream may stay quiet before the receiver is
/// taken to have finished reporting. MX Master 4 (and other devices that may
/// be asleep) need a generous window to wake and respond to the arrival
/// ping; we err on the side of waiting.
const ARRIVAL_DRAIN_TIMEOUT: Duration = Duration::from_millis(1500);

/// A Unifying receiver can transiently stall the first arrival-trigger write
/// while its previous scan settles. Retry once inside the same probe instead
/// of making the inventory ledger treat that single write as a dead channel.
const UNIFYING_TRIGGER_RETRY_DELAY: Duration = Duration::from_millis(300);

/// One device-arrival trigger addresses the receiver itself and should ACK
/// immediately. Keep each attempt well inside the enclosing receiver probe
/// budget so a slow write still retains its liveness-aware probe verdict.
const UNIFYING_TRIGGER_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(750);

/// Receiver register operations are normally answered in a few milliseconds.
/// Keep a stalled liveness/notification request from consuming the enclosing
/// receiver probe budget, so a responsive channel can still report an
/// `AliveButIncomplete` arrival replay instead of becoming an ordinary probe
/// timeout.
const RECEIVER_OPERATION_TIMEOUT: Duration = Duration::from_millis(750);

/// A receiver UID is cache metadata rather than a liveness gate. Give it a
/// shorter window so a delayed serial-number read cannot crowd out the arrival
/// replay and feature-walk budgets.
const RECEIVER_UID_TIMEOUT: Duration = Duration::from_millis(500);

/// Maximum number of pairing slots a Bolt receiver supports. We iterate this
/// range to surface paired-but-offline devices that won't fire arrival events.
const MAX_BOLT_SLOTS: u8 = 6;

/// Upper bound on probing one HID node's I/O. `hidpp`'s request/response has
/// no timeout of its own, so without this a single unresponsive (e.g. asleep)
/// device wedges the whole enumeration, so a permanent hang would stall every
/// later event or recovery reconciliation. Time spent waiting for the node's
/// register phase is not I/O and sits outside it — see [`ProbeTimeouts`].
///
/// A timed-out node is skipped and re-probed by the bounded two-second repair
/// deadline, and the first probe usually wakes the device so the retry succeeds
/// fast.
/// Slots are probed concurrently on both receiver paths, so a receiver's worst
/// case is the 1.5 s arrival drain plus a single slot's [`BOLT_SLOT_PROBE_TIMEOUT`] /
/// [`UNIFYING_SLOT_PROBE_TIMEOUT`] — not their sum — plus, on Bolt only, the
/// sequential pairing-register pass that precedes the slot walk. This stays
/// comfortably above that, so awake devices never trip it.
///
/// Sized for the Bluetooth-direct feature walk, the long pole: a ~35-entry
/// table over a link that drops individual reports, which `hidpp::device`
/// re-asks for per entry. At 6 s one lost report consumed the whole budget and
/// the walk was abandoned mid-table, surfacing as a mouse that never appeared.
const PROBE_TIMEOUT: Duration = Duration::from_secs(25);

/// Probe budget for receiver nodes (Bolt/Unifying/Lightspeed dongles).
///
/// The 25 s [`PROBE_TIMEOUT`] is sized for Bluetooth-direct feature walks that
/// receivers never perform. Keeping the receiver budget tighter matters
/// because a full-budget timeout is also the detection path for a channel
/// whose input-report delivery died (observed on macOS with concurrent opens
/// of the same node: requests keep being written and answered, but the
/// replies are delivered only to the other open handle). Until the channel is
/// replaced every write on it stalls — DPI, SmartShift, ring haptics — so
/// this budget bounds that outage.
///
/// It must still fit a receiver probe's real worst case, which is NOT the
/// millisecond register reads but a paired device's full HID++ 2.0 feature
/// walk: 1.5 s arrival drain + the sequential pairing-register pass + one
/// slot's [`BOLT_SLOT_PROBE_TIMEOUT`] (10 s). 6 s proved too tight — a legitimate
/// deep walk tripped the dead-delivery eviction, the surfaced-empty inventory
/// tore down capture plans, and a pinned stale channel Arc then deadlocked
/// recovery (dead buttons until restart). 13 s clears the honest worst case
/// — and only that: the wait for the receiver's register phase, up to
/// [`host_lock::RECEIVER_REGISTER_TIMEOUT`] on its own, is taken before this
/// budget starts (see [`ProbeTimeouts`]), or the two together would trip
/// it on a working receiver.
const RECEIVER_PROBE_TIMEOUT: Duration = Duration::from_secs(13);

/// Per-slot budget for the HID++ 2.0 feature walk on a Unifying paired device.
///
/// Unifying wireless round-trips are slower than Bolt BTLE: some devices (e.g.
/// K540) take ~3 s for the version ping to return. Running multiple slow slots
/// concurrently can still consume the full PROBE_TIMEOUT and get cancelled
/// mid-walk — the probe returns nothing rather than partial features.  A
/// per-slot cap ensures each slot's feature walk is bounded independently of
/// how many other slots are being probed at the same time.  A timed-out slot
/// still surfaces in the inventory (kind + wpid from the arrival event) — it
/// just lacks capabilities / battery until the next reconciliation.
pub(super) const UNIFYING_SLOT_PROBE_TIMEOUT: Duration = Duration::from_millis(3500);

/// Per-slot budget when a Unifying device already has a fresh immutable probe.
///
/// This path normally performs just one battery read. Some Lightspeed devices
/// occasionally omit that reply even though their receiver has just emitted a
/// live device-arrival event. Do not let that optional refresh consume the
/// full first-sight feature-walk budget or delay publication of a known-online
/// mouse on every reconciliation.
pub(super) const UNIFYING_CACHED_SLOT_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Per-slot budget for the HID++ 2.0 feature walk on a Bolt paired device.
///
/// Bounds a single device that stops answering its feature-walk reads (seen on
/// a recent macOS IOHID stack with a new MX Master 4) so it falls back to its
/// cached / identity-only data instead of pinning its slot future forever
/// (#218). Slots walk *concurrently* (mirroring the Unifying path), so this
/// budget covers the slowest single slot rather than dividing [`PROBE_TIMEOUT`]
/// across the slot count. A healthy walk is not always fast either: a
/// feature-rich device enumerates a large table one round-trip per feature
/// (the MX Master 4's 45 features take ~1–1.6 s over Bolt even awake), and on
/// high-latency USB paths (a Bolt receiver behind a KVM's USB emulation) it
/// takes several seconds — the previous 3 s cap starved every slot there, so a
/// newly paired device could never acquire model info at all. 10 s is generous
/// headroom for degraded-but-alive paths while still fitting [`PROBE_TIMEOUT`]
/// after the 1.5 s arrival drain and Bolt's sequential pairing-register pass.
const BOLT_SLOT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The timeouts one probe pass runs under, kept together so their
/// composition — which waits sit inside which budget — is one place to read,
/// and one value for a test to shrink.
///
/// The composition: a receiver probe waits for the node's register phase for
/// up to `register_lock` *before* its `receiver` timeout starts, and under that
/// budget runs an `arrival_drain` and slot walks each bounded by their own slot
/// probe timeout. A direct device runs under `direct` alone.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProbeTimeouts {
    /// How long a receiver probe waits for another OpenLogi process to
    /// release the node's register phase before settling as deferred
    /// ([`probe::ProbeVerdict::Deferred`]). Outside the I/O budget.
    ///
    /// [`probe::ProbeVerdict::Deferred`]: ProbeVerdict::Deferred
    pub(crate) register_lock: Duration,
    /// [`RECEIVER_PROBE_TIMEOUT`].
    pub(crate) receiver: Duration,
    /// [`PROBE_TIMEOUT`].
    pub(crate) direct: Duration,
    /// [`ARRIVAL_DRAIN_TIMEOUT`].
    pub(crate) arrival_drain: Duration,
    /// [`BOLT_SLOT_PROBE_TIMEOUT`].
    pub(crate) bolt_slot_probe: Duration,
    /// [`UNIFYING_SLOT_PROBE_TIMEOUT`].
    pub(crate) unifying_slot_probe: Duration,
    /// [`UNIFYING_CACHED_SLOT_PROBE_TIMEOUT`].
    pub(crate) unifying_cached_slot_probe: Duration,
}

impl ProbeTimeouts {
    /// The production timeouts.
    pub(crate) const DEFAULT: Self = Self {
        register_lock: host_lock::RECEIVER_REGISTER_TIMEOUT,
        receiver: RECEIVER_PROBE_TIMEOUT,
        direct: PROBE_TIMEOUT,
        arrival_drain: ARRIVAL_DRAIN_TIMEOUT,
        bolt_slot_probe: BOLT_SLOT_PROBE_TIMEOUT,
        unifying_slot_probe: UNIFYING_SLOT_PROBE_TIMEOUT,
        unifying_cached_slot_probe: UNIFYING_CACHED_SLOT_PROBE_TIMEOUT,
    };
}

/// What every probe of one pass shares: the cache it reads, the pass's
/// clock, the event sink, and the timeouts it runs under.
#[derive(Clone, Copy)]
pub(super) struct PassContext<'a> {
    pub(super) cache: &'a HashMap<CacheKey, Cached>,
    pub(super) now: Instant,
    pub(super) subscriptions: Option<&'a EventSubscriptionHandle>,
    pub(super) timeouts: &'a ProbeTimeouts,
}

/// One node probe's verdict about its own trustworthiness. An enum on
/// purpose: the old `healthy`/`complete` bool pair could also express
/// "couldn't check, but the check is complete", which no probe path means —
/// the invariant lived in a comment at every construction site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProbeVerdict {
    /// The node could not be checked (budget timeout, unanswered registers, a
    /// feature walk that never finished): the ledger replays the last-good
    /// snapshot instead of presenting the failure as truth.
    Failed,
    /// The receiver answered a liveness register, but the only operation that
    /// can produce an authoritative device list failed. The ledger may replay
    /// its last-good snapshot briefly, but must eventually reopen the channel.
    AliveButIncomplete,
    /// The node was not checked at all: another OpenLogi process held its
    /// receiver register phase, so this probe skipped the node's I/O and has
    /// no evidence either way. The ledger replays the last-good snapshot
    /// without counting a failure — a channel that was never asked cannot
    /// have failed, and must not be retired for it — and the node's cache
    /// entries are held out of miss aging, while the one-shot retry
    /// re-probes as it would after a failure.
    Deferred,
    /// The node produced an authoritative inventory — the only verdict that
    /// counts as stability evidence. `complete` reports whether every expected
    /// device was seen, which is what lets the one-shot retry stop early.
    Healthy {
        /// Every expected device is present in this probe's inventory.
        complete: bool,
    },
}

impl ProbeVerdict {
    /// `Healthy` with `complete` decided by the walk, `Failed` otherwise —
    /// for paths where one flag carries both facts.
    pub(super) fn healthy_when(answered_in_full: bool) -> Self {
        if answered_in_full {
            Self::Healthy { complete: true }
        } else {
            Self::Failed
        }
    }

    /// The node produced an authoritative inventory this tick.
    pub(super) fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    /// The node was never asked this tick: another process held it.
    pub(super) fn is_deferred(self) -> bool {
        matches!(self, Self::Deferred)
    }

    /// Every expected device was seen (the one-shot retry's stop signal).
    pub(super) fn is_complete(self) -> bool {
        matches!(self, Self::Healthy { complete: true })
    }
}

/// One probed node's contribution this tick: its inventory (if any), the
/// [`ProbeVerdict`] the ledger and the one-shot retry act on (see
/// [`super::ledger::NodeLedger::settle`]), and each device's cache
/// contribution for the caller to apply and to drive eviction.
pub(super) struct NodeProbe {
    pub(super) inventory: Option<DeviceInventory>,
    pub(super) verdict: ProbeVerdict,
    pub(super) outcomes: Vec<CacheOutcome>,
}

impl NodeProbe {
    /// A probe that got no answer at all (budget timeout).
    pub(super) fn failed() -> Self {
        Self {
            inventory: None,
            verdict: ProbeVerdict::Failed,
            outcomes: Vec::new(),
        }
    }

    /// A Unifying receiver that answered `count_pairings` but rejected the
    /// synthetic arrival trigger remains usable for existing control capture.
    fn arrival_replay_failed() -> Self {
        Self {
            inventory: None,
            verdict: ProbeVerdict::AliveButIncomplete,
            outcomes: Vec::new(),
        }
    }

    /// A probe that never ran: another process held the receiver's register
    /// phase for longer than it was willing to wait.
    pub(super) fn deferred() -> Self {
        Self {
            inventory: None,
            verdict: ProbeVerdict::Deferred,
            outcomes: Vec::new(),
        }
    }
}

/// Probe one open HID++ node (channel reused across ticks by the caller),
/// under the pass's timeouts.
///
/// A receiver's probe first takes the node's register phase
/// ([`host_lock::lock_receiver_registers`]) — or settles as
/// [`ProbeVerdict::Deferred`] when another OpenLogi process still holds it
/// after [`ProbeTimeouts::register_lock`] — and only then starts its
/// I/O budget. The wait is time spent not talking to the receiver, so it
/// must not count against the budget the receiver's real worst case was
/// sized for: taken together, a four-second wait plus a legitimate deep
/// slot walk would have tripped the budget, and a budget timeout is a
/// *failure* — two in a row retire the channel — not a deferral.
pub(super) async fn probe_one(
    info: NodeInfo,
    channel: Arc<HidppChannel>,
    pass: PassContext<'_>,
) -> NodeProbe {
    // Receivers answer register reads over local USB in milliseconds; only
    // direct (esp. Bluetooth) devices need the long feature-walk budget. A
    // tight receiver budget bounds the outage when its channel's input-report
    // delivery dies (writes accepted, replies never seen — observed on macOS
    // with concurrent opens of one node).
    let receiver = is_receiver_pid(info.product_id);
    let budget = if receiver {
        pass.timeouts.receiver
    } else {
        pass.timeouts.direct
    };
    match receiver::detect(Arc::clone(&channel)) {
        Some(Receiver::Bolt(bolt)) => {
            let Some(registers) = lock_receiver_registers(&info, pass.timeouts).await else {
                return NodeProbe::deferred();
            };
            within_budget(
                budget,
                receiver,
                probe_bolt_receiver(channel, info, bolt, registers, pass),
            )
            .await
        }
        Some(Receiver::Unifying(unifying)) => {
            let Some(registers) = lock_receiver_registers(&info, pass.timeouts).await else {
                return NodeProbe::deferred();
            };
            within_budget(
                budget,
                receiver,
                probe_unifying_receiver(channel, info, unifying, registers, pass),
            )
            .await
        }
        None | Some(_) => {
            // No recognised receiver — this might be a directly-paired device
            // (Bluetooth-direct, USB-C cable). HID++ at device-index 0xff
            // addresses the device's own features. Probe in case it answers.
            // P2.4 — verified path; no Bolt-pairing slot indirection needed.
            within_budget(budget, receiver, probe_direct(channel, &info, pass)).await
        }
    }
}

/// Take the receiver's register phase for this probe, or `None` to defer it.
async fn lock_receiver_registers(
    info: &NodeInfo,
    timeouts: &ProbeTimeouts,
) -> Option<ReceiverRegisterPhase> {
    host_lock::lock_receiver_registers(&info.id, timeouts.register_lock).await
}

/// Bound a probe's device I/O by `budget`. Burning the whole budget — an
/// asleep direct device, or a channel whose input-report delivery died
/// (writes accepted, replies never seen) — is "couldn't check", not
/// "nothing there": a failed probe, for the ledger to replay through.
async fn within_budget(
    budget: Duration,
    receiver: bool,
    probe: impl Future<Output = NodeProbe>,
) -> NodeProbe {
    if let Ok(probe) = timeout(budget, probe).await {
        return probe;
    }
    warn!(
        ?budget,
        receiver, "device probe timed out — treating as a failed probe"
    );
    NodeProbe::failed()
}
