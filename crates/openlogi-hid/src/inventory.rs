//! Enumerate connected HID++ receivers and their paired devices.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures_concurrency::future::Join as _;
use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::hires_wheel::HiResWheelFeature,
    feature::{
        CreatableFeature, device_information::DeviceInformationFeature,
        device_type_and_name::DeviceTypeAndNameFeature, unified_battery::UnifiedBatteryFeature,
    },
    receiver::{
        self, Receiver,
        bolt::{
            DeviceConnection as BoltDeviceConnection, Event as BoltEvent, Receiver as BoltReceiver,
        },
        unifying::{
            DeviceConnection as UnifyingDeviceConnection, Event as UnifyingEvent,
            Receiver as UnifyingReceiver,
        },
    },
};
use openlogi_core::device::{
    BatteryInfo, Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
    PairedDevice, ReceiverInfo,
};
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::mappings::{
    map_battery_level, map_battery_status, map_device_type, map_kind, map_unifying_kind,
    normalize_serial_number, resolve_device_kind,
};
use crate::node_ledger::NodeLedger;
use crate::route::DIRECT_DEVICE_INDEX;
use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

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
/// Kept short so a snapshot settles quickly: a timed-out node is skipped and
/// re-probed on the next watcher tick (~2 s), and the first probe usually wakes
/// the device so the retry succeeds fast. Comfortably above a healthy device's
/// probe time (the Bolt arrival drain alone is 1.5 s), so awake devices never
/// trip it.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

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

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
}

/// How many `enumerate` ticks a device's probe is reused before a fresh read.
/// The expensive part of a probe (the `enumerate_features` feature-table walk)
/// reads *immutable* data — model, capabilities, marketing type — so it never
/// needs re-reading for a known device; the periodic full probe is kept only as
/// a self-healing pass (e.g. a firmware update reshuffling the feature table).
/// The volatile battery does NOT ride this window: cache hits re-read it every
/// tick through the memoized feature index (see [`read_battery`]), so it stays
/// as fresh as it was before the cache existed (#153).
const REFRESH_TICKS: u64 = 15;

/// Stable identity used to memoize a device's probe across `enumerate` ticks.
/// Keyed on the device's *own* identity (never its slot) so a re-paired or
/// moved device can't inherit another device's cached probe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CacheKey {
    /// Bolt: the unit id from the pairing register (cheap, read every tick).
    Bolt { unit_id: [u8; 4] },
    /// Unifying: keyed on the full receiver serial number + pairing slot.
    /// Using the complete serial (not just a prefix) avoids collisions between
    /// two receivers whose serials share a common prefix (e.g. "DA2699E1" and
    /// "DA2604F2" share "DA2").
    UnifyingSlot { receiver_uid: String, slot: u8 },
    /// Direct (Bluetooth/USB): the OS-assigned HID node id (macOS registry-entry
    /// id, Linux dev path, Windows interface path). Unique *per node*, so two
    /// units of the same model never collide, and stable while connected so the
    /// cache still hits across ticks.
    Direct(async_hid::DeviceId),
}

/// Enumeration ticks a device may be missing before its cache entry is evicted.
/// A small grace rides out a transient receiver timeout without dropping the
/// device's memoized data.
const CACHE_MISS_GRACE: u8 = 3;

/// A memoized probe result plus the tick it was taken on.
#[derive(Clone)]
struct Cached {
    probe: ProbedFeatures,
    /// Runtime index of the `UnifiedBattery` feature in this device's feature
    /// table, captured by the full probe. Lets cache hits re-read the volatile
    /// battery in one round-trip — no `Device::new` ping, no table walk.
    /// `None` when the device exposes no `0x1004`.
    battery_index: Option<u8>,
    probed_tick: u64,
}

/// What a probed device contributes to the cache this tick. The key lets stale
/// entries be evicted; `Fresh` (a full probe) and `Update` (a cache hit whose
/// volatile battery was re-read) also carry the value to insert. `Unkeyed` is a
/// device we can't (or won't) cache — an all-zero unit id, or a rejected
/// non-peripheral — so its key is neither inserted nor kept alive.
enum CacheOutcome {
    Fresh(CacheKey, Cached),
    Update(CacheKey, Cached),
    Seen(CacheKey),
    Unkeyed,
}

/// `Seen` when the device has a stable key, else `Unkeyed`.
fn seen(id: Option<CacheKey>) -> CacheOutcome {
    id.map_or(CacheOutcome::Unkeyed, CacheOutcome::Seen)
}

/// Whether `cached` is stale enough that the device should be re-probed.
fn is_stale(cached: &Cached, tick: u64) -> bool {
    tick.wrapping_sub(cached.probed_tick) >= REFRESH_TICKS
}

/// Decide a device's probe: reuse a fresh cache, or (online + miss/stale)
/// re-probe — but keep the last-known immutable data if the re-probe fails
/// rather than overwriting it with an empty default. An unprobed offline device
/// with no cache yields a default probe. Returns the probe plus its cache
/// contribution (only a *successful* probe is cached).
async fn probe_or_reuse(
    channel: &Arc<HidppChannel>,
    index: u8,
    id: Option<CacheKey>,
    cached: Option<&Cached>,
    online: bool,
    tick: u64,
) -> (ProbedFeatures, CacheOutcome) {
    if online && cached.is_none_or(|c| is_stale(c, tick)) {
        let (fresh, battery_index) = probe_features(channel, index).await;
        // `capabilities` is `Some` exactly when the feature-table walk succeeded;
        // only then is the probe worth caching.
        if fresh.capabilities.is_some() {
            return match id {
                Some(key) => {
                    let value = Cached {
                        probe: fresh.clone(),
                        battery_index,
                        probed_tick: tick,
                    };
                    (fresh, CacheOutcome::Fresh(key, value))
                }
                None => (fresh, CacheOutcome::Unkeyed),
            };
        }
        // Re-probe failed: don't cache the failure. Fall back to the last-known
        // data so a transient glitch doesn't drop the device or its battery.
        // No battery re-read either — the device just proved unresponsive.
        return match cached {
            Some(c) => (c.probe.clone(), seen(id)),
            None => (fresh, seen(id)),
        };
    }
    match cached {
        Some(c) => {
            // Cache hit: the immutable data is reused as-is, but the battery is
            // volatile (#153) — re-read just it through the memoized feature
            // index and fold the reading back into the cache. A failed read
            // (asleep, mid-host-switch) keeps the last-known value.
            if online
                && let Some(feature_index) = c.battery_index
                && let Some(key) = id.clone()
                && let Some(battery) = read_battery(channel, index, feature_index).await
            {
                let mut entry = c.clone();
                entry.probe.battery = Some(battery);
                return (entry.probe.clone(), CacheOutcome::Update(key, entry));
            }
            (c.probe.clone(), seen(id))
        }
        None => (ProbedFeatures::default(), seen(id)),
    }
}

/// Stateful device enumerator: holds the per-device probe cache so the polling
/// watcher reuses immutable data across ticks instead of re-handshaking every
/// device every ~2s. One-shot callers use the [`enumerate`] free function, which
/// runs against a fresh (empty) cache.
#[derive(Default)]
pub struct Enumerator {
    cache: HashMap<CacheKey, Cached>,
    /// Consecutive ticks each cached device has been missing, for grace-period
    /// eviction.
    misses: HashMap<CacheKey, u8>,
    /// Open HID++ channels reused across ticks, keyed by OS node id. Opening (and
    /// tearing down) a device every ~2s tick is the churn issue #99 is about —
    /// each open also leaks an `io_service_t` in async-hid's macOS backend — so a
    /// steadily-connected node is opened once here and reused until it
    /// disconnects.
    channels: HashMap<async_hid::DeviceId, CachedChannel>,
    /// Per-node last-good inventory + consecutive-failure counts: replays a
    /// node's snapshot through transient probe failures and decides when its
    /// cached channel must be dropped and reopened (see [`crate::node_ledger`]).
    ledger: NodeLedger<async_hid::DeviceId>,
    tick: u64,
}

/// An open channel to a receiver / direct-device HID node, held across
/// `enumerate` ticks. Evicting it (on disconnect, or when the `Enumerator`
/// drops) closes the device and joins the channel's read thread via
/// [`HidppChannel`]'s `Drop`.
struct CachedChannel {
    info: async_hid::DeviceInfo,
    channel: Arc<HidppChannel>,
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
    // fail fast, so a short bounded retry is cheap.
    let mut enumerator = Enumerator::default();
    let mut attempt = 1u8;
    loop {
        let (inventories, all_healthy) = enumerator.enumerate_reporting_health().await?;
        if all_healthy || attempt >= ONESHOT_ATTEMPTS {
            return Ok(inventories);
        }
        debug!(
            attempt,
            "one-shot enumerate saw an unhealthy node — retrying"
        );
        tokio::time::sleep(ONESHOT_RETRY_DELAY).await;
        attempt += 1;
    }
}

/// Attempts a one-shot [`enumerate`] makes before returning whatever it last
/// read, when a node keeps coming back unhealthy.
const ONESHOT_ATTEMPTS: u8 = 4;

/// Delay between one-shot [`enumerate`] retries. A first probe usually wakes an
/// asleep device, so a short pause lets the next attempt read it cleanly.
const ONESHOT_RETRY_DELAY: Duration = Duration::from_millis(300);

impl Enumerator {
    /// One enumeration pass, reusing the cache from prior passes. Probes every
    /// HID candidate concurrently (so one asleep node that burns the whole
    /// `PROBE_BUDGET` can't stall the others), reusing each device's cached
    /// immutable data when it's present and fresh.
    ///
    /// A node the OS still lists but whose probe fails (receiver registers
    /// unanswered, probe timeout, open failure) is **not** reported as absent:
    /// its last completed inventory is replayed for a bounded grace and its
    /// channel is reopened, so a transient HID++ glitch can't masquerade as
    /// "no devices" (#218) — see [`crate::node_ledger`].
    pub async fn enumerate(&mut self) -> Result<Vec<DeviceInventory>, InventoryError> {
        self.enumerate_reporting_health().await.map(|(inv, _)| inv)
    }

    /// [`Self::enumerate`] plus whether every probed node answered cleanly this
    /// pass — `false` if any probe timed out, failed to open, or read short of a
    /// receiver's pairing count. The polling watcher ignores the flag (the ledger
    /// already replays a node through a transient miss), but the one-shot
    /// [`enumerate`] free fn uses it to retry: a fresh `Enumerator` has no ledger
    /// history to replay, so a transient miss would otherwise surface as an
    /// empty/partial list (#218).
    async fn enumerate_reporting_health(
        &mut self,
    ) -> Result<(Vec<DeviceInventory>, bool), InventoryError> {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        let candidates = enumerate_hidpp_devices().await?;
        debug!(count = candidates.len(), "HID++ candidate interfaces");

        // Reuse an open channel per node, opening one only for a node seen for
        // the first time. Sequential because opening mutates the channel cache,
        // but in steady state every node is already cached so this is just
        // lookups — an actual open happens only when a new device appears.
        let mut active: Vec<(async_hid::DeviceInfo, Arc<HidppChannel>)> = Vec::new();
        let mut seen_nodes: HashSet<async_hid::DeviceId> = HashSet::new();
        let mut open_failures: Vec<async_hid::DeviceId> = Vec::new();
        for dev in candidates {
            let node = dev.id.clone();
            seen_nodes.insert(node.clone());
            if let Some(open) = self.channels.get(&node) {
                active.push((open.info.clone(), Arc::clone(&open.channel)));
                continue;
            }
            match open_hidpp_channel(dev).await {
                Ok(Some((info, channel))) => {
                    self.channels.insert(
                        node,
                        CachedChannel {
                            info: info.clone(),
                            channel: Arc::clone(&channel),
                        },
                    );
                    active.push((info, channel));
                }
                Ok(None) => {} // speaks HID but not HID++ — not one of ours
                // The node is listed but unreachable right now — settled as a
                // failed probe below, so its last inventory is replayed.
                Err(e) => {
                    warn!(error = ?e, "failed to open HID++ channel — retrying next tick");
                    open_failures.push(node);
                }
            }
        }
        // Drop channels for nodes that vanished this tick. A node missing from
        // the enumeration is a real disconnect (the IOHIDManager device set is
        // authoritative, unlike a HID++ probe timeout), so close the device and
        // join its read thread now instead of leaving a dead channel behind; a
        // reconnect re-opens under a fresh node id. The ledger forgets vanished
        // nodes for the same reason — a true disconnect must not be replayed.
        self.channels.retain(|node, _| seen_nodes.contains(node));
        self.ledger.retain_nodes(&seen_nodes);

        // Probe each open channel concurrently, sharing `&cache` read-only;
        // updates are collected and applied afterwards (no `RefCell`).
        let results = {
            let cache = &self.cache;
            active
                .into_iter()
                .map(|(info, channel)| async move {
                    let node = info.id.clone();
                    let probe = timeout(PROBE_BUDGET, probe_one(info, channel, cache, tick)).await;
                    (node, probe)
                })
                .collect::<Vec<_>>()
                .join()
                .await
        };

        let mut inventories = Vec::new();
        let mut outcomes = Vec::new();
        // Whether every node answered cleanly this pass. Drives the one-shot
        // `enumerate` retry; the ledger's own per-node replay is unaffected.
        let mut all_healthy = true;
        for (node, result) in results {
            let probe = if let Ok(probe) = result {
                probe
            } else {
                // The probe burned the whole budget — an asleep direct device,
                // or a channel whose read loop parked on a dead handle (see
                // `AsyncHidChannel::read_report`). Either way: "couldn't
                // check", not "nothing there".
                warn!(budget = ?PROBE_BUDGET, "device probe timed out — treating as a failed probe");
                NodeProbe::failed()
            };
            all_healthy &= probe.healthy;
            outcomes.extend(probe.outcomes);
            let settled = self.ledger.settle(&node, probe.healthy, probe.inventory);
            if settled.evict_channel && self.channels.remove(&node).is_some() {
                warn!("node probe keeps failing — dropping its channel to reopen next tick");
            }
            inventories.extend(settled.inventory);
        }
        // Nodes that wouldn't open this tick still replay their last snapshot
        // (they have no cached channel to evict).
        for node in open_failures {
            all_healthy = false;
            let settled = self.ledger.settle(&node, false, None);
            inventories.extend(settled.inventory);
        }

        // Apply fresh probes and record which devices were seen this tick.
        let mut seen_keys = HashSet::new();
        for outcome in outcomes {
            match outcome {
                CacheOutcome::Fresh(key, cached) | CacheOutcome::Update(key, cached) => {
                    seen_keys.insert(key.clone());
                    self.cache.insert(key, cached);
                }
                CacheOutcome::Seen(key) => {
                    seen_keys.insert(key);
                }
                CacheOutcome::Unkeyed => {}
            }
        }
        self.evict_unseen(&seen_keys);
        Ok((inventories, all_healthy))
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
            }
        }
    }
}

/// One probed node's contribution this tick: its inventory (if any), whether
/// the node actually answered — the ledger replays the last snapshot when it
/// didn't (see [`NodeLedger::settle`]) — and each device's cache contribution,
/// for the caller to apply and to drive eviction.
struct NodeProbe {
    inventory: Option<DeviceInventory>,
    healthy: bool,
    outcomes: Vec<CacheOutcome>,
}

impl NodeProbe {
    /// A probe that got no answer at all (budget timeout).
    fn failed() -> Self {
        Self {
            inventory: None,
            healthy: false,
            outcomes: Vec::new(),
        }
    }
}

/// Probe one open HID++ node (channel reused across ticks by the caller).
async fn probe_one(
    info: async_hid::DeviceInfo,
    channel: Arc<HidppChannel>,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    match receiver::detect(Arc::clone(&channel)) {
        Some(Receiver::Bolt(bolt)) => probe_bolt_receiver(channel, info, bolt, cache, tick).await,
        Some(Receiver::Unifying(unifying)) => {
            probe_unifying_receiver(channel, info, unifying, cache, tick).await
        }
        None | Some(_) => {
            // No recognised receiver — this might be a directly-paired device
            // (Bluetooth-direct, USB-C cable). HID++ at device-index 0xff
            // addresses the device's own features. Probe in case it answers.
            // P2.4 — verified path; no Bolt-pairing slot indirection needed.
            probe_direct(channel, &info, cache, tick).await
        }
    }
}

async fn probe_bolt_receiver(
    channel: Arc<HidppChannel>,
    info: async_hid::DeviceInfo,
    bolt: BoltReceiver,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    let unique_id = bolt.get_unique_id().await.ok();
    let pairing_count = bolt.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    let connections = drain_device_arrival(&bolt).await;
    debug!(events = connections.len(), "drained device-arrival events");
    let by_slot: HashMap<u8, BoltDeviceConnection> =
        connections.into_iter().map(|c| (c.index, c)).collect();

    let mut paired = Vec::new();
    let mut outcomes = Vec::new();
    for slot in 1u8..=MAX_BOLT_SLOTS {
        if let Some((device, outcome)) =
            probe_bolt_slot(&channel, &bolt, by_slot.get(&slot), slot, cache, tick).await
        {
            paired.push(device);
            outcomes.push(outcome);
        }
    }

    if let Some(count) = pairing_count
        && paired.len() != usize::from(count)
    {
        warn!(
            expected = count,
            found = paired.len(),
            "paired-device count mismatch — some slots may be unreadable"
        );
    }
    // Authoritative only when the pairing-count register answered AND every
    // counted slot was readable. `None` (the receiver didn't answer — e.g. a
    // parked channel) or a shortfall is "couldn't fully check": the ledger
    // then replays the last good snapshot instead of presenting the partial
    // walk as the new truth (#218).
    let complete = pairing_count.is_some_and(|count| paired.len() == usize::from(count));

    NodeProbe {
        inventory: Some(DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                unique_id,
            },
            paired,
        }),
        healthy: complete,
        outcomes,
    }
}

async fn probe_unifying_receiver(
    channel: Arc<HidppChannel>,
    info: async_hid::DeviceInfo,
    unifying: UnifyingReceiver,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    let unique_id = unifying.get_unique_id().await.ok();
    let pairing_count = unifying.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    // Trigger device-arrival events and collect one event per online device.
    // Each event carries the slot index, kind, wpid, and online flag — enough
    // to build a PairedDevice entry for every currently-connected device.
    //
    // Note: the Unifying `0xB5/0x5N` pairing-info register uses a different
    // sub-register base than Bolt, so we don't yet poll offline paired slots.
    // Online devices are covered by the arrival drain; offline device support
    // requires resolving the correct sub-register format.
    //
    // The drain is therefore the *only* device source on this path, so a
    // failed arrival trigger is "couldn't check", not "no devices online":
    // settle it as a failed probe and let the ledger replay the last snapshot.
    let Some(connections) = drain_device_arrival_unifying(&unifying).await else {
        return NodeProbe::failed();
    };
    debug!(events = connections.len(), "drained device-arrival events");

    // Probe all online slots concurrently so a slow HID++ 2.0 feature walk on
    // one device doesn't push the next slot past the PROBE_BUDGET deadline.
    // Pass the receiver UID so each slot's cache key is scoped to this specific
    // receiver — two Unifying receivers sharing a slot number must not share a
    // cache entry (different devices, different capabilities).
    let receiver_uid_fallback;
    let receiver_uid = if let Some(uid) = unique_id.as_deref() {
        uid
    } else {
        // UID fetch failed — use the product ID as a weaker discriminant so
        // two receivers with the same PID still collide, but a receiver and a
        // direct device never share a cache entry.
        tracing::warn!("Unifying receiver UID unavailable; cache isolation may be degraded");
        receiver_uid_fallback = format!("pid:{:04x}", info.product_id);
        &receiver_uid_fallback
    };
    let slot_results = connections
        .iter()
        .map(|conn| probe_unifying_slot(&channel, conn, receiver_uid, cache, tick))
        .collect::<Vec<_>>()
        .join()
        .await;

    let (paired, outcomes): (Vec<_>, Vec<_>) = slot_results.into_iter().flatten().unzip();

    if let Some(count) = pairing_count
        && paired.len() != usize::from(count)
    {
        debug!(
            expected = count,
            found = paired.len(),
            "online devices differ from pairing count; offline devices not yet surfaced for Unifying"
        );
    }
    // Unlike Bolt, a count/list shortfall is *expected* here (offline paired
    // devices aren't enumerable yet), so completeness can't ride on it. The
    // health signal is the pairing-count register answering at all: that
    // proves the receiver round-trip worked this cycle, while `None` (e.g. a
    // parked channel) is "couldn't fully check" — the ledger then replays the
    // last good snapshot instead of presenting a possibly-empty list (#218).
    let healthy = pairing_count.is_some();

    NodeProbe {
        inventory: Some(DeviceInventory {
            receiver: ReceiverInfo {
                name: "Unifying Receiver".to_string(),
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                unique_id,
            },
            paired,
        }),
        healthy,
        outcomes,
    }
}

/// Probe a single Bolt pairing slot. Returns `None` when the slot is empty or
/// unreadable, otherwise the device plus its cache contribution this tick.
async fn probe_bolt_slot(
    channel: &Arc<HidppChannel>,
    bolt: &BoltReceiver,
    event: Option<&BoltDeviceConnection>,
    slot: u8,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> Option<(PairedDevice, CacheOutcome)> {
    let pairing = match bolt.get_device_pairing_information(slot).await {
        Ok(p) => p,
        Err(e) => {
            debug!(slot, error = ?e, "slot empty or unreadable");
            return None;
        }
    };
    let codename = read_codename(channel, slot).await;
    // Prefer event data when present — it's a live response. Fall back to the
    // pairing register for sleeping devices that didn't reply.
    let online = event.map_or(pairing.online, |c| c.online);
    let bolt_kind = event.map_or(pairing.kind, |c| c.kind);
    let wpid = event.map(|c| c.wpid);
    debug!(
        slot,
        online,
        ?wpid,
        ?bolt_kind,
        has_event = event.is_some(),
        codename = ?codename,
        "paired slot"
    );

    // The pairing register gives the device's unit id cheaply every tick — its
    // stable cache identity. An all-zero id is treated as unidentifiable (don't
    // cache; always probe when online).
    let id = (pairing.unit_id != [0u8; 4]).then_some(CacheKey::Bolt {
        unit_id: pairing.unit_id,
    });
    let cached = id.as_ref().and_then(|i| cache.get(i));
    let register_kind = map_kind(bolt_kind);

    let (probe, outcome) = probe_or_reuse(channel, slot, id, cached, online, tick).await;
    if matches!(outcome, CacheOutcome::Fresh(..))
        && let Some(probed) = probe.kind
        && probed != DeviceKind::Unknown
        && register_kind != DeviceKind::Unknown
        && probed != register_kind
    {
        debug!(
            slot,
            ?register_kind,
            ?probed,
            "device-kind sources disagree — trusting 0x0005"
        );
    }

    let device = PairedDevice {
        slot,
        codename,
        wpid,
        // Prefer the device's own `0x0005` type; the register kind is the
        // offline fallback.
        kind: resolve_device_kind(probe.kind, register_kind),
        online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    };
    Some((device, outcome))
}

/// Probe a HID++ channel that doesn't host a Bolt receiver — for
/// Bluetooth-direct, USB-C, or otherwise wired devices that present
/// themselves as a HID++ device rather than a receiver (P2.4).
///
/// Addresses the device at index `0xff` (HID++'s "self" slot) and reads
/// the same battery + model-info features the Bolt path uses. Yields no
/// inventory when the channel doesn't respond to HID++ at `0xff` (in which
/// case it's neither a receiver nor a direct device we recognise) — healthy
/// only if that rejection rests on a completed feature walk, so a device
/// that merely failed to answer is settled as a failed probe instead.
async fn probe_direct(
    channel: Arc<HidppChannel>,
    info: &async_hid::DeviceInfo,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    let id = CacheKey::Direct(info.id.clone());
    let cached = cache.get(&id);
    // A direct device is always "present" (its HID node is the candidate), so
    // treat it as online: reuse the cached probe while fresh, otherwise probe.
    let (probe, outcome) =
        probe_or_reuse(&channel, DIRECT_DEVICE_INDEX, Some(id), cached, true, tick).await;
    // Hybrid peripheral discriminator. A genuine directly-attached device is
    // either wireless/Bluetooth — which reports a battery — or exposes a
    // configuration feature (buttons / pointer / lighting). A Bolt receiver's
    // secondary HID interface also answers DeviceInformation at 0xff, but
    // exposes neither battery nor those features, so it's filtered out here.
    // Without this guard a Bolt setup ends up with two entries in `device_list`:
    // the real mouse (via the Bolt path) and a phantom "direct device" pointing
    // at the receiver, which sits at index 0 and steals every DPI / SmartShift
    // write attempt. We reuse the capabilities the probe already derived from
    // the feature table — no extra round-trip.
    // A completed feature-table walk is what makes this probe's verdict
    // trustworthy: without it (the device never answered) a rejection below
    // would be indistinguishable from a transient glitch, so the node is
    // settled as a failed probe and its last inventory replayed.
    let walk_succeeded = probe.capabilities.is_some();
    let caps = probe.capabilities.unwrap_or_default();
    let is_peripheral = probe.battery.is_some() || caps.buttons || caps.pointer || caps.lighting;
    if !is_peripheral {
        debug!(
            vid = format_args!("{:04x}", info.vendor_id),
            pid = format_args!("{:04x}", info.product_id),
            has_model = probe.model_info.is_some(),
            "slot 0xff exposes no battery or config feature — likely a receiver \
             secondary interface; skipping"
        );
        // Don't cache or keep a rejected non-peripheral — `Unkeyed` lets any
        // prior entry for this node be evicted.
        return NodeProbe {
            inventory: None,
            healthy: walk_succeeded,
            outcomes: vec![CacheOutcome::Unkeyed],
        };
    }

    // Without a Bolt receiver we don't have a wpid, codename, or pairing
    // info — those live on the receiver registers. Use the HID name as
    // the display fallback and leave wpid empty.
    debug!(name = %info.name, "BT-direct / wired device recognised");
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: info.name.clone(),
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some(info.name.clone()),
            wpid: None,
            // No receiver pairing register here, so `0x0005` is the only kind
            // hint — but kind is just identity now; the UI gates on the
            // capabilities below, so a misread kind can't hide the panels (#127).
            kind: resolve_device_kind(probe.kind, DeviceKind::Unknown),
            online: true,
            battery: probe.battery,
            model_info: probe.model_info,
            capabilities: probe.capabilities,
        }],
    };
    NodeProbe {
        inventory: Some(inventory),
        healthy: true,
        outcomes: vec![outcome],
    }
}

async fn drain_device_arrival(bolt: &BoltReceiver) -> Vec<BoltDeviceConnection> {
    let rx = bolt.listen();
    if let Err(e) = bolt.trigger_device_arrival().await {
        debug!(error = ?e, "trigger_device_arrival failed; receiver may report no devices");
        return Vec::new();
    }

    let mut out = Vec::new();
    loop {
        match timeout(ARRIVAL_DRAIN, rx.recv()).await {
            Ok(Ok(BoltEvent::DeviceConnection(c))) => out.push(c),
            Ok(Ok(_)) => {} // BoltEvent is non_exhaustive; ignore future variants
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// `None` when the arrival trigger itself failed: unlike Bolt (whose paired
/// list comes from the slot registers), the drain is the only Unifying device
/// source, so the caller must treat that as a failed probe rather than an
/// empty receiver.
async fn drain_device_arrival_unifying(
    unifying: &UnifyingReceiver,
) -> Option<Vec<UnifyingDeviceConnection>> {
    let rx = unifying.listen();
    if let Err(e) = unifying.trigger_device_arrival().await {
        debug!(error = ?e, "trigger_device_arrival failed; receiver may report no devices");
        return None;
    }

    let mut out = Vec::new();
    loop {
        match timeout(ARRIVAL_DRAIN, rx.recv()).await {
            Ok(Ok(UnifyingEvent::DeviceConnection(c))) => out.push(c),
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    Some(out)
}

/// Probe a Unifying slot from a live device-connection event.
///
/// Device-arrival events carry the slot index, kind, wpid, and online status —
/// enough to surface an entry for every currently-connected device. The
/// unit_id (needed for stable caching across ticks) is not available without a
/// working `get_device_pairing_information` call; we derive a stable cache key
/// from the receiver UID + slot so the feature-table walk is amortised at ~30s
/// and two receivers sharing a slot number don't collide in the cache.
async fn probe_unifying_slot(
    channel: &Arc<HidppChannel>,
    event: &UnifyingDeviceConnection,
    receiver_uid: &str,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> Option<(PairedDevice, CacheOutcome)> {
    let slot = event.index;
    let codename = read_codename(channel, slot).await;
    debug!(
        slot,
        online = event.online,
        wpid = format_args!("{:04x}", event.wpid),
        kind = ?event.kind,
        codename = ?codename,
        "unifying paired slot"
    );

    // Cache key: full receiver serial + slot so two Unifying receivers with
    // a device on the same slot number never share a cache entry.
    let id = CacheKey::UnifyingSlot {
        receiver_uid: receiver_uid.to_string(),
        slot,
    };
    let cached = cache.get(&id);
    let register_kind = map_unifying_kind(event.kind);

    let probe_result = timeout(
        UNIFYING_SLOT_PROBE,
        probe_or_reuse(channel, slot, Some(id.clone()), cached, event.online, tick),
    )
    .await;
    let (probe, outcome) = if let Ok(r) = probe_result {
        r
    } else {
        debug!(slot, budget = ?UNIFYING_SLOT_PROBE,
            "Unifying slot probe timed out; using cached data if available");
        let probe = cached.map_or_else(ProbedFeatures::default, |c| c.probe.clone());
        (probe, CacheOutcome::Seen(id))
    };

    let device = PairedDevice {
        slot,
        codename,
        wpid: Some(event.wpid),
        kind: resolve_device_kind(probe.kind, register_kind),
        online: event.online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    };
    Some((device, outcome))
}

/// Reads a paired device's codename, working around a slicing bug in
/// `hidpp 0.2`'s `BoltReceiver::get_device_codename` that truncates names
/// longer than 8 characters (it treats `response[2]` as an end-index when it
/// is actually the byte length — see Solaar's `device_codename` for the
/// correct slice). 16-byte long-register response is `[sub, chunk, len,
/// data..13]`; we cap at 13 to stay in-bounds. Long names (>13 chars) would
/// need multi-chunk reads with chunk param > 0x01; not needed for v0.0.x.
async fn read_codename(channel: &HidppChannel, slot: u8) -> Option<String> {
    // 0xFF = receiver device index, 0xB5 = ReceiverInfo register,
    // 0x60+slot = DeviceCodename sub-register, 0x01 = first chunk.
    let response = channel
        .read_long_register(0xFF, 0xB5, [0x60 + slot, 0x01, 0x00])
        .await
        .ok()?;
    let len = usize::from(response[2]).min(13);
    core::str::from_utf8(&response[3..3 + len])
        .ok()
        .map(str::to_string)
}

/// Everything a single device probe yields. Any field is `None` when the
/// device doesn't expose that feature or the read failed.
#[derive(Default, Clone)]
struct ProbedFeatures {
    battery: Option<BatteryInfo>,
    model_info: Option<DeviceModelInfo>,
    /// Marketing type from HID++ `0x0005` — an identity hint only.
    kind: Option<DeviceKind>,
    /// Configuration capabilities derived from the device's feature table.
    capabilities: Option<Capabilities>,
}

/// Read just the battery by addressing the `UnifiedBattery` feature at its
/// known runtime `feature_index` — one round-trip, with no `Device::new` ping
/// and no feature-table walk. This is both the full probe's battery read (the
/// walk just produced the index) and the cheap per-tick refresh for cache hits.
/// `None` when the device doesn't answer (asleep, switched hosts).
async fn read_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    feature_index: u8,
) -> Option<BatteryInfo> {
    let feature = UnifiedBatteryFeature::new(Arc::clone(channel), slot, feature_index);
    feature
        .get_battery_info()
        .await
        .ok()
        .map(|info| BatteryInfo {
            percentage: info.charging_percentage,
            level: map_battery_level(info.level),
            status: map_battery_status(info.status),
        })
}

/// Runtime index of the `UnifiedBattery` feature in an enumerated feature-ID
/// table, for [`read_battery`]. The table is 1-based (index 0 is the implicit
/// root feature, which enumeration omits).
fn battery_feature_index(ids: impl IntoIterator<Item = u16>) -> Option<u8> {
    ids.into_iter()
        .position(|id| id == UnifiedBatteryFeature::ID)
        // A feature table holds at most `u8::MAX` entries (its count is a u8),
        // so the 1-based index always fits.
        .and_then(|pos| u8::try_from(pos + 1).ok())
}

/// Open a HID++ session for `slot` and read everything we care about (battery,
/// device-information, `0x0005` device type, and the feature table that drives
/// [`Capabilities`]) in one shot. Device sessions are expensive (multi-round-
/// trip) so we fold every read through the same `Device::new` +
/// `enumerate_features` — the feature table is the Vec that enumeration already
/// returns, so capabilities cost no extra round-trip.
///
/// Also returns the `UnifiedBattery` runtime index found by the walk, so later
/// ticks can refresh the battery without repeating it.
///
/// Only online, responsive devices reach here.
async fn probe_features(channel: &Arc<HidppChannel>, slot: u8) -> (ProbedFeatures, Option<u8>) {
    let mut device = match Device::new(Arc::clone(channel), slot).await {
        Ok(d) => d,
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            return (ProbedFeatures::default(), None);
        }
    };
    // The enumeration response IS the device's feature-ID table — capture it
    // for capability derivation instead of discarding it.
    let mut battery_index = None;
    let mut capabilities = match device.enumerate_features().await {
        Ok(Some(features)) => {
            let ids: Vec<u16> = features.iter().map(|f| f.id).collect();
            battery_index = battery_feature_index(ids.iter().copied());
            Some(Capabilities::from_feature_ids(&ids))
        }
        Ok(None) => None,
        Err(e) => {
            debug!(slot, error = ?e, "enumerate_features failed");
            return (ProbedFeatures::default(), None);
        }
    };
    if let Some(caps) = capabilities.as_mut()
        && let Some(feature) = device.get_feature::<HiResWheelFeature>()
    {
        caps.scroll_inversion = feature
            .get_wheel_capabilities()
            .await
            .is_ok_and(|wheel| wheel.has_invert);
    }

    let battery = match battery_index {
        Some(feature_index) => read_battery(channel, slot, feature_index).await,
        None => None,
    };

    let model_info = match device.get_feature::<DeviceInformationFeature>() {
        Some(feature) => match feature.get_device_info().await {
            Ok(info) => {
                let serial_number = if info.capabilities.serial_number {
                    match feature.get_serial_number().await {
                        Ok(serial) => normalize_serial_number(&serial),
                        Err(e) => {
                            debug!(slot, error = ?e, "DeviceInformation serial read failed");
                            None
                        }
                    }
                } else {
                    None
                };
                Some(DeviceModelInfo {
                    entity_count: info.entity_count,
                    serial_number,
                    unit_id: info.unit_id,
                    transports: DeviceTransports {
                        usb: info.transport.usb,
                        equad: info.transport.e_quad,
                        btle: info.transport.btle,
                        bluetooth: info.transport.bluetooth,
                    },
                    model_ids: info.model_id,
                    extended_model_id: info.extended_model_id,
                })
            }
            Err(e) => {
                debug!(slot, error = ?e, "DeviceInformation read failed");
                None
            }
        },
        None => None,
    };

    // `0x0005` reports the device's own marketing type (mouse, keyboard, …) —
    // the authoritative kind signal. On the direct path it's the only one; on
    // the Bolt path it corrects a pairing register that reported the wrong (or
    // `Unknown`) kind.
    let kind = match device.get_feature::<DeviceTypeAndNameFeature>() {
        Some(feature) => match feature.get_device_type().await {
            Ok(ty) => Some(map_device_type(ty)),
            Err(e) => {
                debug!(slot, error = ?e, "DeviceType read failed");
                None
            }
        },
        None => None,
    };

    (
        ProbedFeatures {
            battery,
            model_info,
            kind,
            capabilities,
        },
        battery_index,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        CACHE_MISS_GRACE, CacheKey, Cached, Enumerator, ProbedFeatures, REFRESH_TICKS,
        UnifiedBatteryFeature, battery_feature_index, is_stale,
    };
    use hidpp::feature::CreatableFeature as _;

    fn cache_entry(probed_tick: u64) -> Cached {
        Cached {
            probe: ProbedFeatures::default(),
            battery_index: None,
            probed_tick,
        }
    }

    #[test]
    fn cache_entry_survives_grace_then_evicts() {
        let mut e = Enumerator::default();
        let key = CacheKey::Bolt {
            unit_id: [1, 2, 3, 4],
        };
        e.cache.insert(key.clone(), cache_entry(0));
        let nobody = HashSet::new();
        // Missing for the whole grace window: kept.
        for _ in 0..CACHE_MISS_GRACE {
            e.evict_unseen(&nobody);
            assert!(
                e.cache.contains_key(&key),
                "evicted inside the grace window"
            );
        }
        // One miss past the grace: evicted.
        e.evict_unseen(&nobody);
        assert!(
            !e.cache.contains_key(&key),
            "should evict past the grace window"
        );
    }

    #[test]
    fn being_seen_resets_the_miss_counter() {
        let mut e = Enumerator::default();
        let key = CacheKey::Bolt { unit_id: [9; 4] };
        e.cache.insert(key.clone(), cache_entry(0));
        let nobody = HashSet::new();
        let seen: HashSet<CacheKey> = std::iter::once(key.clone()).collect();
        e.evict_unseen(&nobody); // miss 1
        e.evict_unseen(&seen); // seen → counter reset
        for _ in 0..CACHE_MISS_GRACE {
            e.evict_unseen(&nobody);
        }
        assert!(
            e.cache.contains_key(&key),
            "counter reset by a sighting, so still within grace"
        );
    }

    #[test]
    fn cached_probe_is_reused_until_refresh_ticks() {
        let cached = Cached {
            probe: ProbedFeatures::default(),
            battery_index: None,
            probed_tick: 10,
        };
        assert!(!is_stale(&cached, 10), "same tick is fresh");
        assert!(
            !is_stale(&cached, 10 + REFRESH_TICKS - 1),
            "just under the window is still fresh"
        );
        assert!(
            is_stale(&cached, 10 + REFRESH_TICKS),
            "at the window the probe is refreshed"
        );
    }

    #[test]
    fn battery_index_is_one_based_in_the_enumerated_table() {
        // `enumerate_features` omits the root feature (index 0), so the first
        // enumerated entry sits at runtime index 1.
        let table = [0x0001, UnifiedBatteryFeature::ID, 0x2201];
        assert_eq!(battery_feature_index(table), Some(2));
        assert_eq!(
            battery_feature_index([UnifiedBatteryFeature::ID]),
            Some(1),
            "first entry maps to index 1, not 0"
        );
    }

    #[test]
    fn no_battery_feature_means_no_index() {
        assert_eq!(battery_feature_index([0x0001, 0x2201, 0x1b04]), None);
        assert_eq!(battery_feature_index([]), None);
    }
}
