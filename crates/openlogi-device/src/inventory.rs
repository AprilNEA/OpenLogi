//! Enumerate connected HID++ receivers and their paired devices.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    hash::Hash,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_concurrency::future::Join as _;
use hidpp::channel::HidppChannel;
use openlogi_core::device::DeviceInventory;
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::ChannelRegistry;
use crate::backend::{BackendError, HidBackend, NodeId, NodeInfo};
use crate::channel::route::{DeviceRoute, find_receiver, is_receiver_pid};
use ledger::NodeLedger;

mod cache;
pub mod events;
mod features;
pub mod hotplug;
mod ledger;
mod mappings;
pub mod persist;
mod probe;
pub mod standalone;

use cache::{CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached};
use events::{ChannelEventSubscriptions, EventNotifier, EventSubscriptionHandle};
use persist::{ProbeCacheSnapshot, ProbeCacheStore};
use probe::{NodeProbe, probe_one};

/// How long to wait for device-arrival event bursts before assuming the
/// receiver has finished reporting. MX Master 4 (and other devices that may
/// be asleep) need a generous window to wake and respond to the arrival
/// ping; we err on the side of waiting.
const ARRIVAL_DRAIN: Duration = Duration::from_millis(1500);

/// Maximum number of pairing slots a Bolt receiver supports. We iterate this
/// range to surface paired-but-offline devices that won't fire arrival events.
const MAX_BOLT_SLOTS: u8 = 6;

/// Upper bound on probing one HID node. `hidpp`'s request/response has no
/// timeout of its own, so without this a single unresponsive (e.g. asleep)
/// device wedges the whole enumeration, so a permanent hang would stall every
/// later event or recovery reconciliation.
///
/// A timed-out node is skipped and re-probed by the bounded two-second repair
/// deadline, and the first probe usually wakes the device so the retry succeeds
/// fast.
/// Slots are probed concurrently on both receiver paths, so a receiver's worst
/// case is the 1.5 s arrival drain plus a single slot's [`BOLT_SLOT_PROBE`] /
/// [`UNIFYING_SLOT_PROBE`] — not their sum — plus, on Bolt only, the
/// sequential pairing-register pass that precedes the slot walk. This stays
/// comfortably above that, so awake devices never trip it.
///
/// Sized for the Bluetooth-direct feature walk, the long pole: a ~35-entry
/// table over a link that drops individual reports, which `hidpp::device`
/// re-asks for per entry. At 6 s one lost report consumed the whole budget and
/// the walk was abandoned mid-table, surfacing as a mouse that never appeared.
const PROBE_BUDGET: Duration = Duration::from_secs(25);

/// Probe budget for receiver nodes (Bolt/Unifying/Lightspeed dongles).
///
/// The 25 s [`PROBE_BUDGET`] is sized for Bluetooth-direct feature walks that
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
/// slot's [`BOLT_SLOT_PROBE`] (10 s). 6 s proved too tight — a legitimate
/// deep walk tripped the dead-delivery eviction, the surfaced-empty inventory
/// tore down capture plans, and a pinned stale channel Arc then deadlocked
/// recovery (dead buttons until restart). 13 s clears the honest worst case.
const RECEIVER_PROBE_BUDGET: Duration = Duration::from_secs(13);

/// Per-slot budget for the HID++ 2.0 feature walk on a Unifying paired device.
///
/// Unifying wireless round-trips are slower than Bolt BTLE: some devices (e.g.
/// K540) take ~3 s for the version ping to return. Running multiple slow slots
/// concurrently can still consume the full PROBE_BUDGET and get cancelled
/// mid-walk — the probe returns nothing rather than partial features.  A
/// per-slot cap ensures each slot's feature walk is bounded independently of
/// how many other slots are being probed at the same time.  A timed-out slot
/// still surfaces in the inventory (kind + wpid from the arrival event) — it
/// just lacks capabilities / battery until the next reconciliation.
const UNIFYING_SLOT_PROBE: Duration = Duration::from_millis(3500);

/// Per-slot budget when a Unifying device already has a fresh immutable probe.
///
/// This path normally performs just one battery read. Some Lightspeed devices
/// occasionally omit that reply even though their receiver has just emitted a
/// live device-arrival event. Do not let that optional refresh consume the
/// full first-sight feature-walk budget or delay publication of a known-online
/// mouse on every reconciliation.
const UNIFYING_CACHED_SLOT_PROBE: Duration = Duration::from_millis(750);

/// Per-slot budget for the HID++ 2.0 feature walk on a Bolt paired device.
///
/// Bounds a single device that stops answering its feature-walk reads (seen on
/// a recent macOS IOHID stack with a new MX Master 4) so it falls back to its
/// cached / identity-only data instead of pinning its slot future forever
/// (#218). Slots walk *concurrently* (mirroring the Unifying path), so this
/// budget covers the slowest single slot rather than dividing [`PROBE_BUDGET`]
/// across the slot count. A healthy walk is not always fast either: a
/// feature-rich device enumerates a large table one round-trip per feature
/// (the MX Master 4's 45 features take ~1–1.6 s over Bolt even awake), and on
/// high-latency USB paths (a Bolt receiver behind a KVM's USB emulation) it
/// takes several seconds — the previous 3 s cap starved every slot there, so a
/// newly paired device could never acquire model info at all. 10 s is generous
/// headroom for degraded-but-alive paths while still fitting [`PROBE_BUDGET`]
/// after the 1.5 s arrival drain and Bolt's sequential pairing-register pass.
const BOLT_SLOT_PROBE: Duration = Duration::from_secs(10);

/// Errors raised while enumerating HID++ devices.
#[derive(Debug, Error)]
pub enum InventoryError {
    /// Underlying HID backend error.
    #[error("HID transport error")]
    Hid(#[from] BackendError),
    /// More than one indistinguishable standalone raw-HID node was found.
    #[error("multiple indistinguishable standalone raw HID devices found")]
    AmbiguousRawDevice,
}

/// Stateful device enumerator: owns persistent channels and the per-device
/// probe cache so the event-first watcher reuses immutable data across
/// reconciliations. One-shot callers use the [`enumerate`] free function, which
/// runs against a fresh (empty) cache.
pub struct Enumerator {
    /// The HID stack this enumerator walks. `openlogi-hid` supplies this
    /// host's; tests and other hosts supply their own.
    backend: Arc<dyn HidBackend>,
    cache: HashMap<CacheKey, Cached>,
    /// Consecutive passes each cached device has been missing, for grace-period
    /// eviction.
    misses: HashMap<CacheKey, u8>,
    /// Open HID++ channels reused across reconciliations, keyed by OS node id.
    /// Opening (and tearing down) a device every pass is the churn issue #99 is about —
    /// each open also leaks an `io_service_t` in async-hid's macOS backend — so a
    /// steadily-connected node is opened once here and reused until it
    /// disconnects.
    channels: ChannelCache<NodeId, CachedChannel>,
    /// Per-node last-good inventory + consecutive-failure counts: replays a
    /// node's snapshot through transient probe failures and decides when its
    /// cached channel must be dropped and reopened (see [`crate::inventory::ledger`]).
    ledger: NodeLedger<NodeId>,
    /// Optional publication sink used by the persistent Agent watcher. One-shot
    /// callers keep this `None` and retain the route-opening library behavior.
    registry: Option<ChannelRegistry>,
    /// Where the immutable probe cache is kept across restarts, `None` for a
    /// memory-only enumerator (one-shot CLI calls, tests).
    store: Option<Arc<dyn ProbeCacheStore>>,
    /// Whether the persistable cache content changed since the last save —
    /// fresh full probes and evictions, not per-pass battery refreshes.
    cache_dirty: bool,
    /// Whether the most recent pass failed to open at least one HID++ node.
    open_failures_last_tick: bool,
    /// Whether the most recent pass needs a bounded fast follow-up to advance
    /// ledger/channel/cache grace or retry a failed open.
    retry_needed_last_tick: bool,
    /// Coalesced lifecycle-event sink installed on newly opened channels.
    event_notifier: Option<EventNotifier>,
}

/// An open channel to a receiver / direct-device HID node, held across
/// `enumerate` ticks. Evicting it (on disconnect, or when the `Enumerator`
/// drops) closes the device and joins the channel's read thread via
/// [`HidppChannel`]'s `Drop`.
struct CachedChannel {
    info: NodeInfo,
    channel: Arc<HidppChannel>,
    events: Option<ChannelEventSubscriptions>,
}

type ActiveNode = (NodeInfo, Arc<HidppChannel>, Option<EventSubscriptionHandle>);

struct PreparedNodes {
    active: Vec<ActiveNode>,
    open_failures: Vec<NodeId>,
    retiring: Vec<NodeId>,
}

/// One channel's place in its lifecycle: serving, or retired and draining
/// toward quiescence so its node may reopen.
enum ChannelState<Channel> {
    Active(Channel),
    Retiring(Channel),
}

/// Per-node channel lifecycle, generic so ownership transitions can be tested
/// without constructing a platform HID node. One map on purpose: a node
/// holding an active *and* a retiring channel — two live opens of one OS
/// node, the state the macOS dead-delivery hazard rides on — used to be
/// representable across two maps and guarded only by a release-silent
/// `debug_assert`; keyed by node in a single map, it cannot exist.
struct ChannelCache<Node, Channel> {
    channels: HashMap<Node, ChannelState<Channel>>,
}

impl<Node, Channel> Default for ChannelCache<Node, Channel> {
    fn default() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }
}

impl<Node: Eq + Hash + Clone, Channel> ChannelCache<Node, Channel> {
    fn get(&self, node: &Node) -> Option<&Channel> {
        match self.channels.get(node)? {
            ChannelState::Active(channel) => Some(channel),
            ChannelState::Retiring(_) => None,
        }
    }

    /// The active channels, for callers that sweep what is currently serving.
    fn active_iter(&self) -> impl Iterator<Item = (&Node, &Channel)> {
        self.channels
            .iter()
            .filter_map(|(node, state)| match state {
                ChannelState::Active(channel) => Some((node, channel)),
                ChannelState::Retiring(_) => None,
            })
    }

    fn insert(&mut self, node: Node, channel: Channel) {
        match self.channels.entry(node) {
            Entry::Occupied(mut entry) => match entry.get_mut() {
                state @ ChannelState::Active(_) => *state = ChannelState::Active(channel),
                // A retiring channel is still draining toward quiescence, and
                // `prepare_open` refuses the node until it has — so this arm
                // is unreachable through current callers. Keeping the
                // draining channel (and dropping the newcomer, which closes
                // its OS handle promptly) preserves the one-channel-per-node
                // invariant either way.
                ChannelState::Retiring(_) => {
                    warn!("refusing to replace a retiring channel — dropping the fresh open");
                    drop(channel);
                }
            },
            Entry::Vacant(entry) => {
                entry.insert(ChannelState::Active(channel));
            }
        }
    }

    /// Move an active channel into the retiring state. `false` when the node
    /// is unknown or already retiring (the original retirement keeps its
    /// place — and its drain clock).
    fn retire_node(&mut self, node: &Node) -> bool {
        let Some(state) = self.channels.get_mut(node) else {
            return false;
        };
        if matches!(state, ChannelState::Retiring(_)) {
            return false;
        }
        let Some(ChannelState::Active(channel) | ChannelState::Retiring(channel)) =
            self.channels.remove(node)
        else {
            return false;
        };
        self.channels
            .insert(node.clone(), ChannelState::Retiring(channel));
        true
    }

    /// Whether this node may be opened during the current tick. A quiescent
    /// retirement is dropped here, but opening remains deferred to a later tick.
    fn prepare_open(&mut self, node: &Node, is_quiescent: impl FnOnce(&Channel) -> bool) -> bool {
        let Some(ChannelState::Retiring(channel)) = self.channels.get(node) else {
            return true;
        };
        if is_quiescent(channel) {
            self.channels.remove(node);
        }
        false
    }

    /// Retire every active node not in `seen`; returns how many retired.
    fn retire_absent(&mut self, seen: &HashSet<Node>) -> usize {
        let absent = self
            .active_iter()
            .map(|(node, _)| node)
            .filter(|node| !seen.contains(*node))
            .cloned()
            .collect::<Vec<_>>();
        absent
            .into_iter()
            .filter(|node| self.retire_node(node))
            .count()
    }

    fn reap_absent(&mut self, seen: &HashSet<Node>, is_quiescent: impl Fn(&Channel) -> bool) {
        self.channels.retain(|node, state| match state {
            ChannelState::Active(_) => true,
            ChannelState::Retiring(channel) => seen.contains(node) || !is_quiescent(channel),
        });
    }

    #[cfg(test)]
    fn is_retiring(&self, node: &Node) -> bool {
        matches!(self.channels.get(node), Some(ChannelState::Retiring(_)))
    }
}

fn routes_for_inventories(inventories: &[DeviceInventory]) -> Vec<DeviceRoute> {
    inventories
        .iter()
        .flat_map(|inventory| {
            inventory
                .paired
                .iter()
                .filter_map(|paired| DeviceRoute::device_route_for(inventory, paired.slot))
        })
        .collect()
}

fn settle_unhealthy_node<Node: Eq + Hash + Clone>(
    ledger: &mut NodeLedger<Node>,
    node: &Node,
    all_complete: &mut bool,
    all_healthy: &mut bool,
) -> Option<DeviceInventory> {
    *all_complete = false;
    *all_healthy = false;
    ledger.settle(node, false, None).inventory
}

/// Enumerate all Logitech HID++ receivers visible to the current process and
/// the devices paired to each.
///
/// Combines two data sources per receiver:
///
/// - `trigger_device_arrival` events — the only path to a device's wireless
///   PID in hidpp 0.2 (the `wpid` field on `BoltDevicePairingInformation` is
///   private). Only online, responsive devices show up here.
/// - `get_device_pairing_information` polled per slot — covers paired-but-
///   offline devices (sleeping mice, devices on a different host) that the
///   arrival ping doesn't wake. No wpid for these.
///
/// We merge the two so an MX Master that's been asleep still shows up with
/// its codename and kind even before you click it.
pub async fn enumerate(
    backend: Arc<dyn HidBackend>,
) -> Result<Vec<DeviceInventory>, InventoryError> {
    // The persistent [`Enumerator`] keeps a per-node ledger across passes, so a
    // transient probe miss replays the node's last good inventory. A one-shot
    // caller (CLI `list` / `diag`) builds a fresh `Enumerator` whose ledger is
    // empty, so a miss has nothing to replay and would surface as an empty or
    // partial list — the two isolated runs in #218 read 3 devices and 0. Retry a
    // few times instead, reusing the same enumerator so its ledger accumulates a
    // snapshot a later attempt can replay and the opened channel stays warm.
    // #226's 5 s request timeout inside `HidppChannel::send` makes a dead probe
    // fail fast, so a short bounded retry is cheap. Some transports can answer
    // while still yielding a short device set (for example, a Unifying arrival
    // event landing just after the drain window). When every node answered this
    // cycle but that healthy pass is still short, two identical inventories mean
    // the expected stable Unifying offline drain has settled. A failed/timed-out
    // probe must keep using the full retry budget so the next attempt can reopen
    // the channel and recover.
    let mut enumerator = Enumerator::with_backend(backend);
    let mut scan = OneShotScan::new();
    loop {
        let (inventories, all_complete, all_healthy) =
            enumerator.enumerate_reporting_completeness().await?;
        let pass = ScanPass {
            complete: all_complete,
            healthy: all_healthy,
        };
        if scan.is_settled(&inventories, pass) {
            return Ok(inventories);
        }
        debug!(
            attempt = scan.attempt,
            complete = pass.complete,
            healthy = pass.healthy,
            "one-shot enumerate inventory incomplete or still changing — retrying"
        );
        scan.advance(inventories, pass);
        tokio::time::sleep(ONESHOT_RETRY_DELAY).await;
    }
}

/// What one enumerate pass established about its own trustworthiness.
#[derive(Clone, Copy)]
struct ScanPass {
    /// Every expected device is present in the snapshot.
    complete: bool,
    /// Every probe answered — the only kind of pass that counts as stability
    /// evidence.
    healthy: bool,
}

/// The one-shot retry loop's memory: the snapshot that may serve as stability
/// evidence, and how many passes have run. [`Self::is_settled`] is the stop
/// rule the tests pin.
struct OneShotScan {
    /// The previous pass's snapshot, kept only when that pass was healthy —
    /// the unchanged-inventory stop only ever compares two consecutive
    /// healthy snapshots. A failed/timed-out probe (a replayed last-good or
    /// partial live result) is cleared so it can't count as one of the two
    /// "stable" reads and short-circuit a later healthy-but-short pass.
    stable_candidate: Option<Vec<DeviceInventory>>,
    attempt: u8,
}

impl OneShotScan {
    fn new() -> Self {
        Self {
            stable_candidate: None,
            attempt: 1,
        }
    }

    /// Stop when the snapshot is complete, when a healthy but short pass has
    /// stabilized (the expected Unifying offline-drain case), or when the
    /// attempt cap is reached.
    fn is_settled(&self, current: &[DeviceInventory], pass: ScanPass) -> bool {
        pass.complete
            || (pass.healthy
                && self
                    .stable_candidate
                    .as_deref()
                    .is_some_and(|previous| previous == current))
            || self.attempt >= ONESHOT_ATTEMPTS
    }

    /// Fold one finished, unsettled pass in: a healthy snapshot becomes the
    /// stability candidate, an unhealthy one clears it.
    fn advance(&mut self, inventories: Vec<DeviceInventory>, pass: ScanPass) {
        self.stable_candidate = pass.healthy.then_some(inventories);
        self.attempt += 1;
    }
}

/// Attempts a one-shot [`enumerate`] makes before returning whatever it last
/// read, when an inventory keeps coming back incomplete or changing.
const ONESHOT_ATTEMPTS: u8 = 4;

/// Delay between one-shot [`enumerate`] retries. A first probe usually wakes an
/// asleep device, so a short pause lets the next attempt read it cleanly.
const ONESHOT_RETRY_DELAY: Duration = Duration::from_millis(300);

/// Nodes that remain valid for this pass: everything the OS enumerated plus
/// cached channels whose open transport still reports a live connection.
fn retained_nodes<K>(
    enumerated: &HashSet<K>,
    cached_channels: impl IntoIterator<Item = (K, bool)>,
) -> HashSet<K>
where
    K: Clone + Eq + Hash,
{
    let mut retained = enumerated.clone();
    retained.extend(
        cached_channels
            .into_iter()
            .filter_map(|(node, connected)| connected.then_some(node)),
    );
    retained
}

/// Add cached channels omitted by this OS enumeration while their open
/// transport still reports a live connection.
fn append_live_cached_channels(
    nodes: &mut HashSet<NodeId>,
    channels: &ChannelCache<NodeId, CachedChannel>,
    active: &mut Vec<ActiveNode>,
) {
    let retained = retained_nodes(
        nodes,
        channels
            .active_iter()
            .map(|(node, open)| (node.clone(), open.channel.is_connected())),
    );
    for node in retained.difference(nodes) {
        if let Some(open) = channels.get(node) {
            debug!(
                ?node,
                name = %open.info.name,
                "OS enumeration omitted a live HID node; probing cached channel"
            );
            active.push((
                open.info.clone(),
                Arc::clone(&open.channel),
                open.events.as_ref().map(ChannelEventSubscriptions::handle),
            ));
        }
    }
    *nodes = retained;
}

impl Enumerator {
    /// Whether the most recent [`enumerate`](Self::enumerate) pass failed to
    /// open at least one HID++ node. `false` before the first pass.
    ///
    /// On macOS a run of passes with this set is the observable signature of a
    /// denied Input Monitoring grant or a stale permission session — paired
    /// with the grant state it separates "grant it" from "log out", which the
    /// bare open error cannot (the denial is silent).
    #[must_use]
    pub fn open_failures_last_tick(&self) -> bool {
        self.open_failures_last_tick
    }

    /// Whether the last pass needs a bounded fast follow-up to advance a
    /// probe/channel/cache recovery policy. `false` before the first pass.
    #[must_use]
    pub fn retry_needed_last_tick(&self) -> bool {
        self.retry_needed_last_tick
    }

    /// An enumerator that walks `backend` — this host's HID stack, a scripted
    /// device tree in tests, or another host's.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn HidBackend>) -> Self {
        Self {
            backend,
            cache: HashMap::new(),
            misses: HashMap::new(),
            channels: ChannelCache::default(),
            ledger: NodeLedger::default(),
            registry: None,
            store: None,
            cache_dirty: false,
            open_failures_last_tick: false,
            retry_needed_last_tick: false,
            event_notifier: None,
        }
    }

    /// Publish this enumerator's already-open channels into `registry` after
    /// each settled inventory pass.
    #[must_use]
    pub fn with_registry(mut self, registry: ChannelRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Subscribe this enumerator's already-open channels to lifecycle events
    /// and coalesce them into reconciliation requests sent through `notifier`.
    #[must_use]
    pub fn with_event_notifier(mut self, notifier: EventNotifier) -> Self {
        self.event_notifier = Some(notifier);
        self
    }

    /// Warm-start this enumerator's immutable probe cache from `store`, and
    /// write it back there whenever its persistable content changes.
    ///
    /// A modifier rather than a constructor: persistence is orthogonal to the
    /// channel registry and the backend, so an enumerator can carry all three.
    #[must_use]
    pub fn with_probe_cache(mut self, store: Arc<dyn ProbeCacheStore>) -> Self {
        let cache = store.load().into_entries();
        if !cache.is_empty() {
            debug!(entries = cache.len(), "probe cache warm-started");
        }
        self.cache.extend(cache);
        self.store = Some(store);
        self
    }

    async fn prepare_nodes(
        &mut self,
        backend: &dyn HidBackend,
        candidates: Vec<NodeInfo>,
    ) -> PreparedNodes {
        let mut active = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut open_failures = Vec::new();
        let mut retiring = Vec::new();
        for info in candidates {
            let node = info.id.clone();
            seen_nodes.insert(node.clone());
            if !self
                .channels
                .prepare_open(&node, |cached| Arc::strong_count(&cached.channel) == 1)
            {
                debug!("node still retiring — waiting for its channel's remaining users to drop");
                retiring.push(node);
                continue;
            }
            if let Some(open) = self.channels.get(&node) {
                active.push((
                    open.info.clone(),
                    Arc::clone(&open.channel),
                    open.events.as_ref().map(ChannelEventSubscriptions::handle),
                ));
                continue;
            }
            match backend.open_hidpp(&info).await {
                Ok(Some(channel)) => {
                    // Attach before the first feature/register check. Receiver
                    // events are recognizable immediately; per-device feature
                    // indexes are registered during the ensuing table walk.
                    let events = self.event_notifier.as_ref().map(|notifier| {
                        let protocol = find_receiver(info.vendor_id, info.product_id)
                            .map(|receiver| receiver.protocol);
                        ChannelEventSubscriptions::attach(&channel, protocol, notifier.clone())
                    });
                    let event_handle = events.as_ref().map(ChannelEventSubscriptions::handle);
                    self.channels.insert(
                        node,
                        CachedChannel {
                            info: info.clone(),
                            channel: Arc::clone(&channel),
                            events,
                        },
                    );
                    active.push((info, channel, event_handle));
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(error = ?e, "failed to open HID++ channel — requesting repair");
                    open_failures.push(node);
                }
            }
        }

        // IOHIDManager can temporarily omit a Bluetooth device's vendor HID++
        // collection while its already-open handle and ordinary mouse link are
        // still live. Keep probing that cached channel instead of turning one
        // incomplete OS snapshot into an offline device and stopping capture.
        append_live_cached_channels(&mut seen_nodes, &self.channels, &mut active);

        if let Some(registry) = &self.registry {
            registry.retain_nodes(&seen_nodes);
        }
        self.channels.retire_absent(&seen_nodes);
        self.channels.reap_absent(&seen_nodes, |cached| {
            Arc::strong_count(&cached.channel) == 1
        });
        self.ledger.retain_nodes(&seen_nodes);

        PreparedNodes {
            active,
            open_failures,
            retiring,
        }
    }

    /// Write the cache through to its store when the persistable content
    /// changed this pass. Best-effort: a failed write is logged and retried on
    /// the next cache-dirty pass.
    fn flush_cache(&mut self) {
        if !self.cache_dirty {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        match store.save(&ProbeCacheSnapshot::of(&self.cache)) {
            Ok(()) => self.cache_dirty = false,
            Err(e) => warn!(error = %e, "failed to persist probe cache"),
        }
    }

    /// One enumeration pass, reusing the cache from prior passes. Probes every
    /// HID candidate concurrently (so one asleep node that burns the whole
    /// `PROBE_BUDGET` can't stall the others), reusing each device's cached
    /// immutable data when it's present and fresh.
    ///
    /// A node the OS still lists but whose probe fails (receiver registers
    /// unanswered, probe timeout, open failure) is **not** reported as absent:
    /// its last completed inventory is replayed for a bounded grace and its
    /// channel is reopened, so a transient HID++ glitch can't masquerade as
    /// "no devices" (#218) — see the node ledger.
    pub async fn enumerate(&mut self) -> Result<Vec<DeviceInventory>, InventoryError> {
        self.enumerate_reporting_completeness()
            .await
            .map(|(inv, _, _)| inv)
    }

    /// [`Self::enumerate`] plus whether every probed node produced a complete
    /// enough snapshot for the one-shot caller to stop early, and whether every
    /// probed node answered this cycle. Completeness is separate from per-node
    /// health: a node can answer cleanly enough for the ledger to accept its
    /// live inventory while still reporting a known count/list shortfall that
    /// the one-shot retry should give one more chance to settle. Only healthy
    /// shortfalls can use the unchanged-inventory early stop; failed probes must
    /// run through the retry budget so a later attempt can recover.
    async fn enumerate_reporting_completeness(
        &mut self,
    ) -> Result<(Vec<DeviceInventory>, bool, bool), InventoryError> {
        let now = Instant::now();
        let backend = Arc::clone(&self.backend);
        let candidates = backend.enumerate_hidpp().await?;
        debug!(count = candidates.len(), "HID++ candidate interfaces");

        // Reuse an open channel per node, opening only when no active or
        // retiring connection owns that OS node.
        let PreparedNodes {
            active,
            open_failures,
            retiring: retiring_nodes,
        } = self.prepare_nodes(&*backend, candidates).await;
        self.open_failures_last_tick = !open_failures.is_empty();

        // Probe each open channel concurrently, sharing `&cache` read-only;
        // updates are collected and applied afterwards (no `RefCell`).
        let results = {
            let cache = &self.cache;
            active
                .into_iter()
                .map(|(info, channel, events)| async move {
                    let node = info.id.clone();
                    // Receivers answer register reads over local USB in
                    // milliseconds; only direct (esp. Bluetooth) devices need
                    // the long feature-walk budget. A tight receiver budget
                    // bounds the outage when its channel's input-report
                    // delivery dies (writes accepted, replies never seen —
                    // observed on macOS with concurrent opens of one node).
                    let receiver = is_receiver_pid(info.product_id);
                    let budget = if receiver {
                        RECEIVER_PROBE_BUDGET
                    } else {
                        PROBE_BUDGET
                    };
                    let probe = timeout(
                        budget,
                        probe_one(info, Arc::clone(&channel), cache, now, events.as_ref()),
                    )
                    .await;
                    (node, channel, probe, budget, receiver)
                })
                .collect::<Vec<_>>()
                .join()
                .await
        };

        let mut inventories = Vec::new();
        let mut outcomes = Vec::new();
        // Aggregates for the one-shot retry. `all_complete` can stop
        // immediately; `all_healthy` gates the unchanged-inventory shortcut so
        // failed probes keep retrying. The ledger's own per-node replay is
        // governed by each probe's verdict.
        let mut all_complete = true;
        let mut all_healthy = true;
        for (node, channel, result, budget, receiver) in results {
            let probe = if let Ok(probe) = result {
                probe
            } else {
                // The probe burned the whole budget — an asleep direct device,
                // or a channel whose input-report delivery died (writes
                // accepted, replies never seen). Either way: "couldn't
                // check", not "nothing there".
                warn!(
                    ?budget,
                    receiver, "device probe timed out — treating as a failed probe"
                );
                NodeProbe::failed()
            };
            all_complete &= probe.verdict.is_complete();
            all_healthy &= probe.verdict.is_healthy();
            outcomes.extend(probe.outcomes);
            let settled = self
                .ledger
                .settle(&node, probe.verdict.is_healthy(), probe.inventory);
            // Every node waits for the ledger's consecutive-failure threshold,
            // receivers included. One full-budget timeout is not evidence of
            // dead delivery: [`RECEIVER_PROBE_BUDGET`] leaves barely a second
            // over its own documented worst case, so a legitimate deep walk
            // plus a single lost reply (5 s `SEND_RESPONSE_TIMEOUT`) already
            // exceeds it. Evicting on that unpublishes *every* device behind
            // the receiver — a Bolt publishes all six slots under one node —
            // and tears down each one's capture plan. A channel whose delivery
            // really is dead times out again on the repair pass and is replaced
            // then, with the ledger replaying its last-good inventory
            // meanwhile, so nothing disappears from the GUI in between.
            if settled.evict_channel {
                if let Some(registry) = &self.registry {
                    registry.remove_node(&node);
                }
                if self.channels.retire_node(&node) {
                    warn!("node probe keeps failing — retiring its channel before reopen");
                }
            } else if let Some(registry) = &self.registry {
                let routes = settled
                    .inventory
                    .as_ref()
                    .map_or_else(Vec::new, |inventory| {
                        routes_for_inventories(std::slice::from_ref(inventory))
                    });
                if routes.is_empty() {
                    registry.remove_node(&node);
                } else {
                    registry.replace_node(node.clone(), routes, channel);
                }
            }
            inventories.extend(settled.inventory);
        }
        // A listed node whose old connection is still retiring is an unhealthy
        // probe, not a disconnect: preserve the ledger's normal replay grace.
        for node in retiring_nodes {
            inventories.extend(settle_unhealthy_node(
                &mut self.ledger,
                &node,
                &mut all_complete,
                &mut all_healthy,
            ));
        }
        // Nodes that wouldn't open this pass still replay their last snapshot
        // (they have no cached channel to evict).
        for node in open_failures {
            inventories.extend(settle_unhealthy_node(
                &mut self.ledger,
                &node,
                &mut all_complete,
                &mut all_healthy,
            ));
        }

        let seen_keys = self.apply_outcomes(outcomes);
        self.evict_unseen(&seen_keys);
        self.retry_needed_last_tick = !all_healthy || !self.misses.is_empty();
        self.flush_cache();
        Ok((inventories, all_complete, all_healthy))
    }

    /// Fold this pass's probe outcomes into the cache, returning the keys seen
    /// so [`Self::evict_unseen`] can age out the rest.
    fn apply_outcomes(&mut self, outcomes: Vec<CacheOutcome>) -> HashSet<CacheKey> {
        let mut seen_keys = HashSet::new();
        for outcome in outcomes {
            match outcome {
                CacheOutcome::Fresh(key, cached) => {
                    seen_keys.insert(key.clone());
                    // A completed full probe of a persistable device is worth
                    // writing through; battery `Update`s are not (they would
                    // rewrite the file every pass for a value that is re-read
                    // live anyway), and neither are keys `persist::save`
                    // filters out — dirtying on those would rewrite an
                    // unchanged file on every refresh of a direct-only system.
                    self.cache_dirty |= persist::is_persistable(&key);
                    self.cache.insert(key, cached);
                }
                CacheOutcome::Update(key, cached) => {
                    seen_keys.insert(key.clone());
                    self.cache.insert(key, cached);
                }
                CacheOutcome::Seen(key) => {
                    seen_keys.insert(key);
                }
                CacheOutcome::Unkeyed => {}
            }
        }
        seen_keys
    }

    /// Drop cache entries for devices not seen this pass, after a short grace so
    /// a transient receiver timeout doesn't discard a still-present device.
    fn evict_unseen(&mut self, seen_keys: &HashSet<CacheKey>) {
        for key in seen_keys {
            self.misses.remove(key);
        }
        let missing: Vec<CacheKey> = self
            .cache
            .keys()
            .filter(|k| !seen_keys.contains(*k))
            .cloned()
            .collect();
        for key in missing {
            let misses = self.misses.entry(key.clone()).or_insert(0);
            *misses += 1;
            if *misses > CACHE_MISS_GRACE {
                self.cache.remove(&key);
                self.misses.remove(&key);
                self.cache_dirty |= persist::is_persistable(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests;
