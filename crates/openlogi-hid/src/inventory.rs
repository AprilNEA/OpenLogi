//! Enumerate connected HID++ receivers and their paired devices.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures_concurrency::future::Join as _;
use hidpp::channel::HidppChannel;
use openlogi_core::device::DeviceInventory;
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::node_ledger::NodeLedger;
use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

mod cache;
mod features;
mod probe;

use cache::{CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached};
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
/// Kept short so a snapshot settles quickly: a timed-out node is skipped and
/// re-probed on the next watcher tick (~2 s), and the first probe usually wakes
/// the device so the retry succeeds fast. Slots are probed concurrently on both
/// receiver paths, so a healthy receiver's worst case is the 1.5 s arrival drain
/// plus a single slot's [`BOLT_SLOT_PROBE`] / [`UNIFYING_SLOT_PROBE`] — not their
/// sum — which this stays comfortably above, so awake devices never trip it.
const PROBE_BUDGET: Duration = Duration::from_secs(6);

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
/// Without a per-slot cap a single online device that stops answering its
/// feature-walk reads burns the whole receiver's [`PROBE_BUDGET`], so
/// `probe_one` times out and the receiver yields *nothing* — every paired device
/// drops to "No devices" even though its pairing-register identity read fine
/// (#218). Capping each slot lets a hung device fall back to its cached /
/// identity-only data while the rest of the receiver still enumerates, mirroring
/// [`UNIFYING_SLOT_PROBE`].
///
/// Bolt slots are probed concurrently (see `probe_bolt_receiver`), so this cap
/// bounds each slot independently and does *not* sum across slots — the receiver
/// cycle is the arrival drain plus the single slowest slot. 3 s is generous
/// headroom for a healthy walk: a feature-rich device enumerates a large table
/// one round-trip per feature, and the MX Master 4 (45 features over Bolt) takes
/// ~1–1.6 s even awake. The previous 1 s cap cut that walk off every tick, so
/// the device surfaced permanently with no capabilities or battery.
const BOLT_SLOT_PROBE: Duration = Duration::from_secs(3);

/// Errors raised while enumerating HID++ devices.
#[derive(Debug, Error)]
pub enum InventoryError {
    /// Underlying HID backend error.
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
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
        // Aggregates for the one-shot retry. `all_complete` can stop
        // immediately; `all_healthy` gates the unchanged-inventory shortcut so
        // failed probes keep retrying. The ledger's own per-node replay is
        // governed by `probe.healthy`.
        let mut all_complete = true;
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
            all_complete &= probe.complete;
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
            all_complete = false;
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
        Ok((inventories, all_complete, all_healthy))
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

#[cfg(test)]
mod tests;
