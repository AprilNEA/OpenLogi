//! Enumerate connected HID++ receivers and their paired devices.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
    time::Duration,
};

use futures_concurrency::future::Join as _;
use hidpp::channel::HidppChannel;
use openlogi_core::device::DeviceInventory;
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::ChannelRegistry;
use crate::backend::{BackendError, HidBackend, NodeId, NodeInfo};
use crate::channel::route::{DeviceRoute, is_receiver_pid};
use crate::channel::transport::native_backend;
use ledger::NodeLedger;

mod cache;
mod features;
pub mod hotplug;
mod ledger;
mod mappings;
pub mod persist;
mod probe;
pub(crate) mod standalone;

use cache::{CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached};
use persist::{FileProbeCacheStore, ProbeCacheSnapshot, ProbeCacheStore};
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
/// device wedges the whole enumeration — and the GUI runs `enumerate` on a
/// polling watcher, so a permanent hang would stall every later refresh.
///
/// A timed-out node is skipped and re-probed on the next watcher tick (~2 s),
/// and the first probe usually wakes the device so the retry succeeds fast.
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
/// just lacks capabilities / battery until the next tick.
const UNIFYING_SLOT_PROBE: Duration = Duration::from_millis(3500);

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

/// Stateful device enumerator: holds the per-device probe cache so the polling
/// watcher reuses immutable data across ticks instead of re-handshaking every
/// device every ~2s. One-shot callers use the [`enumerate`] free function, which
/// runs against a fresh (empty) cache.
pub struct Enumerator {
    /// The HID stack this enumerator walks. Defaults to the host's; tests and
    /// non-native hosts supply their own through [`Enumerator::with_backend`].
    backend: Arc<dyn HidBackend>,
    cache: HashMap<CacheKey, Cached>,
    /// Consecutive ticks each cached device has been missing, for grace-period
    /// eviction.
    misses: HashMap<CacheKey, u8>,
    /// Open HID++ channels reused across ticks, keyed by OS node id. Opening (and
    /// tearing down) a device every ~2s tick is the churn issue #99 is about —
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
    tick: u64,
    /// Where the immutable probe cache is kept across restarts, `None` for a
    /// memory-only enumerator (one-shot CLI calls, tests).
    store: Option<Arc<dyn ProbeCacheStore>>,
    /// Whether the persistable cache content changed since the last save —
    /// fresh full probes and evictions, not per-tick battery refreshes.
    cache_dirty: bool,
}

impl Default for Enumerator {
    fn default() -> Self {
        Self::with_backend(native_backend())
    }
}

/// An open channel to a receiver / direct-device HID node, held across
/// `enumerate` ticks. Evicting it (on disconnect, or when the `Enumerator`
/// drops) signals the channel's reader to stop via [`HidppChannel`]'s `Drop`;
/// the device closes once that task winds down and releases the last handle.
struct CachedChannel {
    info: NodeInfo,
    channel: Arc<HidppChannel>,
}

struct PreparedNodes {
    active: Vec<(NodeInfo, Arc<HidppChannel>)>,
    open_failures: Vec<NodeId>,
    retiring: Vec<NodeId>,
}

/// Disjoint active and retiring channels, generic so ownership transitions can
/// be tested without constructing a platform HID node.
struct ChannelCache<Node, Channel> {
    active: HashMap<Node, Channel>,
    retiring: HashMap<Node, Channel>,
}

impl<Node, Channel> Default for ChannelCache<Node, Channel> {
    fn default() -> Self {
        Self {
            active: HashMap::new(),
            retiring: HashMap::new(),
        }
    }
}

impl<Node: Eq + Hash + Clone, Channel> ChannelCache<Node, Channel> {
    fn get(&self, node: &Node) -> Option<&Channel> {
        self.active.get(node)
    }

    fn insert(&mut self, node: Node, channel: Channel) {
        debug_assert!(!self.retiring.contains_key(&node));
        self.active.insert(node, channel);
    }

    fn retire_node(&mut self, node: &Node) -> Option<&Channel> {
        let channel = self.active.remove(node)?;
        // Overwrite rather than keep an older retirement. Holding a node in
        // both maps is a bug `insert` only debug-asserts against, and the
        // caller uses what comes back to release *this* channel's cache pin —
        // handed the stale one, it would clear the wrong pointer and leave the
        // real pin in place, which is what blocks a node from reopening.
        self.retiring.insert(node.clone(), channel);
        self.retiring.get(node)
    }

    /// Whether this node may be opened during the current tick. A quiescent
    /// retirement is dropped here, but opening remains deferred to a later tick.
    fn prepare_open(&mut self, node: &Node, is_quiescent: impl FnOnce(&Channel) -> bool) -> bool {
        let Some(channel) = self.retiring.get(node) else {
            return true;
        };
        if is_quiescent(channel) {
            self.retiring.remove(node);
        }
        false
    }

    fn retire_absent(&mut self, seen: &HashSet<Node>, mut on_retire: impl FnMut(&Channel)) {
        let absent = self
            .active
            .keys()
            .filter(|node| !seen.contains(*node))
            .cloned()
            .collect::<Vec<_>>();
        for node in absent {
            if let Some(channel) = self.retire_node(&node) {
                on_retire(channel);
            }
        }
    }

    fn reap_absent(&mut self, seen: &HashSet<Node>, is_quiescent: impl Fn(&Channel) -> bool) {
        self.retiring
            .retain(|node, channel| seen.contains(node) || !is_quiescent(channel));
    }

    #[cfg(test)]
    fn is_retiring(&self, node: &Node) -> bool {
        self.retiring.contains_key(node)
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
pub async fn enumerate() -> Result<Vec<DeviceInventory>, InventoryError> {
    // The polling [`Enumerator`] keeps a per-node ledger across ticks, so a
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
    let mut enumerator = Enumerator::default();
    let mut previous_inventories: Option<Vec<DeviceInventory>> = None;
    let mut attempt = 1u8;
    loop {
        let (inventories, all_complete, all_healthy) =
            enumerator.enumerate_reporting_completeness().await?;
        if one_shot_should_stop(
            previous_inventories.as_deref(),
            &inventories,
            all_complete,
            all_healthy,
            attempt,
        ) {
            return Ok(inventories);
        }
        debug!(
            attempt,
            all_complete,
            all_healthy,
            "one-shot enumerate inventory incomplete or still changing — retrying"
        );
        // Only a healthy pass is valid evidence for the unchanged-inventory
        // stop, so the equality check below only ever compares two consecutive
        // healthy snapshots. A failed/timed-out probe (replayed last-good or
        // partial live result) is cleared so it can't count as one of the two
        // "stable" reads and short-circuit a later healthy-but-short pass.
        previous_inventories = if all_healthy { Some(inventories) } else { None };
        tokio::time::sleep(ONESHOT_RETRY_DELAY).await;
        attempt += 1;
    }
}

/// Stop the one-shot retry loop when the snapshot is complete, when a healthy
/// but short pass has stabilized (the expected Unifying offline-drain case), or
/// when the explicit attempt cap is reached. An unchanged inventory from a
/// failed probe is not stable evidence; it must keep retrying until the cap.
fn one_shot_should_stop(
    previous: Option<&[DeviceInventory]>,
    current: &[DeviceInventory],
    all_complete: bool,
    all_healthy: bool,
    attempt: u8,
) -> bool {
    all_complete
        || (all_healthy && previous.is_some_and(|previous| previous == current))
        || attempt >= ONESHOT_ATTEMPTS
}

/// Attempts a one-shot [`enumerate`] makes before returning whatever it last
/// read, when an inventory keeps coming back incomplete or changing.
const ONESHOT_ATTEMPTS: u8 = 4;

/// Delay between one-shot [`enumerate`] retries. A first probe usually wakes an
/// asleep device, so a short pause lets the next attempt read it cleanly.
const ONESHOT_RETRY_DELAY: Duration = Duration::from_millis(300);

/// Nodes that remain valid for this tick: everything the OS enumerated plus
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
    active: &mut Vec<(NodeInfo, Arc<HidppChannel>)>,
) {
    let retained = retained_nodes(
        nodes,
        channels
            .active
            .iter()
            .map(|(node, open)| (node.clone(), open.channel.is_connected())),
    );
    for node in retained.difference(nodes) {
        if let Some(open) = channels.get(node) {
            debug!(
                ?node,
                name = %open.info.name,
                "OS enumeration omitted a live HID node; probing cached channel"
            );
            active.push((open.info.clone(), Arc::clone(&open.channel)));
        }
    }
    *nodes = retained;
}

impl Enumerator {
    /// Build an enumerator that walks `backend` instead of the host's HID
    /// stack — a scripted device tree in tests, or another host's stack.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn HidBackend>) -> Self {
        Self {
            backend,
            cache: HashMap::new(),
            misses: HashMap::new(),
            channels: ChannelCache::default(),
            ledger: NodeLedger::default(),
            registry: None,
            tick: 0,
            store: None,
            cache_dirty: false,
        }
    }

    /// Build a persistent enumerator that publishes its already-open channels
    /// into `registry` after each settled inventory tick.
    #[must_use]
    pub fn with_registry(registry: ChannelRegistry) -> Self {
        Self {
            registry: Some(registry),
            ..Self::default()
        }
    }

    /// Warm-start this enumerator's immutable probe cache from (and write it
    /// back to) the on-disk cache under the app data dir, so a device fully
    /// probed once keeps its identity across restarts. Falls back to
    /// memory-only when no data dir is resolvable.
    ///
    /// A modifier rather than a constructor: persistence is orthogonal to the
    /// channel registry, so the agent's enumerator carries both.
    #[must_use]
    pub fn persisted(self) -> Self {
        match FileProbeCacheStore::in_data_dir() {
            Some(store) => self.with_probe_cache(Arc::new(store)),
            None => self,
        }
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
                active.push((open.info.clone(), Arc::clone(&open.channel)));
                continue;
            }
            match backend.open_hidpp(&info).await {
                Ok(Some(channel)) => {
                    self.channels.insert(
                        node,
                        CachedChannel {
                            info: info.clone(),
                            channel: Arc::clone(&channel),
                        },
                    );
                    active.push((info, channel));
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(error = ?e, "failed to open HID++ channel — retrying next tick");
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
        self.channels.retire_absent(&seen_nodes, |cached| {
            crate::write::clear_haptic_feature_cache_for(&cached.channel);
        });
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
    /// changed this tick. Best-effort: a failed write is logged and retried on
    /// the next dirty tick.
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
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
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

        // Probe each open channel concurrently, sharing `&cache` read-only;
        // updates are collected and applied afterwards (no `RefCell`).
        let results = {
            let cache = &self.cache;
            active
                .into_iter()
                .map(|(info, channel)| async move {
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
                    let probe =
                        timeout(budget, probe_one(info, Arc::clone(&channel), cache, tick)).await;
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
        // governed by `probe.healthy`.
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
            all_complete &= probe.complete;
            all_healthy &= probe.healthy;
            outcomes.extend(probe.outcomes);
            let settled = self.ledger.settle(&node, probe.healthy, probe.inventory);
            // Every node waits for the ledger's consecutive-failure threshold,
            // receivers included. One full-budget timeout is not evidence of
            // dead delivery: [`RECEIVER_PROBE_BUDGET`] leaves barely a second
            // over its own documented worst case, so a legitimate deep walk
            // plus a single lost reply (5 s `SEND_RESPONSE_TIMEOUT`) already
            // exceeds it. Evicting on that unpublishes *every* device behind
            // the receiver — a Bolt publishes all six slots under one node —
            // and tears down each one's capture plan. A channel whose delivery
            // really is dead times out again on the next tick and is replaced
            // then, with the ledger replaying its last-good inventory
            // meanwhile, so nothing disappears from the GUI in between.
            if settled.evict_channel {
                if let Some(registry) = &self.registry {
                    registry.remove_node(&node);
                }
                if let Some(cached) = self.channels.retire_node(&node) {
                    // Release the haptic cache's pin on this channel NOW —
                    // waiting for the next haptic route-miss deadlocks when
                    // capture dies with it (see clear_haptic_feature_cache_for).
                    crate::write::clear_haptic_feature_cache_for(&cached.channel);
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
        // Nodes that wouldn't open this tick still replay their last snapshot
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
        self.flush_cache();
        Ok((inventories, all_complete, all_healthy))
    }

    /// Fold this tick's probe outcomes into the cache, returning the keys seen
    /// so [`Self::evict_unseen`] can age out the rest.
    fn apply_outcomes(&mut self, outcomes: Vec<CacheOutcome>) -> HashSet<CacheKey> {
        let mut seen_keys = HashSet::new();
        for outcome in outcomes {
            match outcome {
                CacheOutcome::Fresh(key, cached) => {
                    seen_keys.insert(key.clone());
                    // A completed full probe of a persistable device is worth
                    // writing through; battery `Update`s are not (they would
                    // rewrite the file every tick for a value that is re-read
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

    /// Drop cache entries for devices not seen this tick, after a short grace so
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
