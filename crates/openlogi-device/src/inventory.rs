//! Enumerate connected HID++ receivers and their paired devices.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_concurrency::future::Join as _;
use hidpp::channel::HidppChannel;
use openlogi_core::device::DeviceInventory;
use thiserror::Error;
use tracing::{debug, warn};

use crate::ChannelRegistry;
use crate::backend::{BackendError, HidBackend, NodeId, NodeInfo};
use crate::channel::route::{DeviceRoute, find_receiver};
use ledger::{NodeLedger, SettledNode};

mod cache;
mod channel_cache;
pub mod events;
mod features;
pub mod hotplug;
mod ledger;
mod mappings;
pub mod persist;
mod probe;
pub mod standalone;

use cache::{CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached};
use channel_cache::ChannelCache;
use events::{ChannelEventSubscriptions, EventNotifier, EventSubscriptionHandle};
use persist::{ProbeCacheSnapshot, ProbeCacheStore};
use probe::{NodeProbe, PassContext, ProbeTimeouts, ProbeVerdict, probe_one};
#[cfg(test)]
use probe::{UNIFYING_CACHED_SLOT_PROBE, UNIFYING_SLOT_PROBE};

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
    /// The cache entries each node has contributed and still holds: what a
    /// deferred tick holds out of miss aging, since a probe that never ran
    /// has seen nothing and missed nothing (see [`Self::evict_unseen`]).
    node_cache_keys: HashMap<NodeId, HashSet<CacheKey>>,
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
    /// The timeouts every pass's probes run under.
    timeouts: ProbeTimeouts,
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

/// Fold one probe into the ledger. A deferred probe never touched the node,
/// so it is replayed without a failure on the ledger's count; its verdict
/// still fails `all_healthy`, which brings the one-shot retry round again.
fn settle_probe<Node: Eq + Hash + Clone>(
    ledger: &mut NodeLedger<Node>,
    node: &Node,
    verdict: ProbeVerdict,
    inventory: Option<DeviceInventory>,
) -> SettledNode {
    match verdict {
        ProbeVerdict::Deferred => ledger.defer(node),
        ProbeVerdict::AliveButIncomplete => ledger.settle_arrival_replay_failure(node),
        verdict => ledger.settle(node, verdict.is_healthy(), inventory),
    }
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
            node_cache_keys: HashMap::new(),
            channels: ChannelCache::default(),
            ledger: NodeLedger::default(),
            registry: None,
            store: None,
            cache_dirty: false,
            open_failures_last_tick: false,
            retry_needed_last_tick: false,
            event_notifier: None,
            timeouts: ProbeTimeouts::DEFAULT,
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
                    // A channel that actually opened must not inherit probe or
                    // arrival-replay eviction counts from its predecessor.
                    // Inventory replay remains bounded until a probe produces
                    // a new authoritative snapshot.
                    self.ledger.reset_channel_failures_after_open(&node);
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
        self.node_cache_keys
            .retain(|node, _| seen_nodes.contains(node));

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
        // updates are collected and applied afterwards (no `RefCell`). Each
        // probe bounds its own I/O by the pass's timeouts (`probe_one`).
        let results = {
            let cache = &self.cache;
            let timeouts = &self.timeouts;
            active
                .into_iter()
                .map(|(info, channel, events)| async move {
                    let node = info.id.clone();
                    let pass = PassContext {
                        cache,
                        now,
                        subscriptions: events.as_ref(),
                        timeouts,
                    };
                    let probe = probe_one(info, Arc::clone(&channel), pass).await;
                    (node, channel, probe)
                })
                .collect::<Vec<_>>()
                .join()
                .await
        };

        let (mut inventories, mut outcomes) = (Vec::new(), Vec::new());
        // Aggregates for the one-shot retry. `all_complete` can stop
        // immediately; `all_healthy` gates the unchanged-inventory shortcut so
        // failed probes keep retrying. The ledger's own per-node replay is
        // governed by each probe's verdict.
        let (mut all_complete, mut all_healthy) = (true, true);
        // Entries of nodes whose probe was deferred: neither seen nor missed
        // this pass.
        let mut frozen_keys = HashSet::new();
        for (node, channel, probe) in results {
            all_complete &= probe.verdict.is_complete();
            all_healthy &= probe.verdict.is_healthy();
            self.hold_or_note_cache_keys(&node, &probe, &mut frozen_keys);
            outcomes.extend(probe.outcomes);
            let settled = settle_probe(&mut self.ledger, &node, probe.verdict, probe.inventory);
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
        self.evict_unseen(&seen_keys, &frozen_keys);
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

    /// Fold one node's probe into the cache's per-node bookkeeping.
    ///
    /// A probe that ran records the entries it contributed, so a later
    /// deferred tick knows which entries are the node's. A deferred probe
    /// adds those to `frozen`: the entries [`Self::evict_unseen`] holds out
    /// of miss aging this pass. A tick that never asked the node has seen
    /// nothing and missed nothing — its empty outcomes are not the node
    /// reporting its devices gone, and four such ticks must not delete the
    /// last-good capabilities the ledger is still replaying the inventory
    /// for, nor persist that deletion.
    ///
    /// A node deferred before this process has successfully probed it has no record
    /// yet, but its entries may well be in the cache: a warm start loads the
    /// persisted Bolt entries before any receiver answers. Every entry no
    /// node has claimed is held for it then — nothing that was checked
    /// contributed them, so nothing that was checked can have found them
    /// missing — and the node's first healthy probe attributes what is its.
    fn hold_or_note_cache_keys(
        &mut self,
        node: &NodeId,
        probe: &NodeProbe,
        frozen: &mut HashSet<CacheKey>,
    ) {
        if probe.verdict.is_deferred() {
            match self.node_cache_keys.get(node) {
                Some(keys) => frozen.extend(keys.iter().cloned()),
                None => frozen.extend(self.unattributed_cache_keys()),
            }
            return;
        }
        // A failed first probe cannot establish cache ownership, even if it
        // found some slots before failing. Keep unknown attribution distinct
        // from a healthy probe that positively found no entries.
        if !probe.verdict.is_healthy() && !self.node_cache_keys.contains_key(node) {
            return;
        }
        let keys = probe.outcomes.iter().filter_map(CacheOutcome::key).cloned();
        self.node_cache_keys
            .entry(node.clone())
            .or_default()
            .extend(keys);
    }

    /// Cache entries no node probed by this process has contributed:
    /// persisted entries loaded at start, until their node's first probe.
    fn unattributed_cache_keys(&self) -> impl Iterator<Item = CacheKey> + '_ {
        self.cache
            .keys()
            .filter(|key| {
                !self
                    .node_cache_keys
                    .values()
                    .any(|keys| keys.contains(*key))
            })
            .cloned()
    }

    /// Drop cache entries for devices not seen this pass, after a short grace so
    /// a transient receiver timeout doesn't discard a still-present device.
    ///
    /// Entries in `frozen` — those of nodes whose probe was deferred — are
    /// neither seen nor missed: their counters stand until the node is
    /// actually probed again.
    fn evict_unseen(&mut self, seen_keys: &HashSet<CacheKey>, frozen: &HashSet<CacheKey>) {
        for key in seen_keys {
            self.misses.remove(key);
        }
        let missing: Vec<CacheKey> = self
            .cache
            .keys()
            .filter(|k| !seen_keys.contains(*k) && !frozen.contains(*k))
            .cloned()
            .collect();
        for key in missing {
            let misses = self.misses.entry(key.clone()).or_insert(0);
            *misses += 1;
            if *misses > CACHE_MISS_GRACE {
                self.cache.remove(&key);
                self.misses.remove(&key);
                for keys in self.node_cache_keys.values_mut() {
                    keys.remove(&key);
                }
                self.cache_dirty |= persist::is_persistable(&key);
            }
        }
    }
}

#[cfg(test)]
mod replay_test_support;
#[cfg(test)]
mod replay_tests;
#[cfg(test)]
mod tests;
