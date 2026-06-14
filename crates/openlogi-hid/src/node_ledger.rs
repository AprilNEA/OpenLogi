//! Per-node probe-health ledger for the [`crate::inventory::Enumerator`].
//!
//! A HID node that the OS still enumerates can stop answering HID++ — a
//! receiver register read times out, or the transport read loop parked on a
//! `Disconnected` handle (see `AsyncHidChannel::read_report`). Without this
//! ledger such a tick yields an empty/partial inventory that is
//! indistinguishable from "checked, no devices", so the GUI flaps between the
//! full device list and "No devices connected" (#218), and a parked channel —
//! which is only ever evicted when its node *vanishes* — wedges enumeration
//! until the agent is restarted.
//!
//! The ledger fixes both: while a node's probe fails it replays the node's
//! last completed inventory for a bounded grace, and after a couple of
//! consecutive failures it asks the enumerator to drop the node's cached
//! channel so the next tick reopens it fresh.
//!
//! Generic over the node key (`async_hid::DeviceId` in production) purely so
//! the decision table is testable on every platform — the id type is
//! `cfg`-gated per OS and not constructible off-target.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use openlogi_core::device::DeviceInventory;
use tracing::{debug, warn};

/// Ticks a node's last-good inventory keeps being served while its probe
/// fails. Past this the live (partial or empty) result is surfaced, so a
/// receiver that is genuinely wedged eventually shows the truth instead of an
/// ever-staler snapshot. Mirrors the probe cache's `CACHE_MISS_GRACE`, so a
/// node recovers with its memoized probes still warm.
const NODE_MISS_GRACE: u8 = 3;

/// Consecutive failed probes after which the node's cached channel should be
/// dropped and reopened. A channel whose read loop parked on a `Disconnected`
/// handle never recovers on its own — the transport contract (see
/// `AsyncHidChannel::read_report`) expects the inventory watcher to evict it,
/// and node-vanish eviction never fires for a node the OS keeps listing.
const CHANNEL_EVICT_AFTER: u8 = 2;

/// What [`NodeLedger::settle`] decided for one node this tick.
pub(crate) struct SettledNode {
    /// The inventory to report for the node: the live result, or the replayed
    /// last-good snapshot while the failure is within grace.
    pub inventory: Option<DeviceInventory>,
    /// Whether the node's cached channel should be dropped so the next tick
    /// reopens it. `true` on every failed tick from [`CHANNEL_EVICT_AFTER`]
    /// onwards, so a persistently sick node keeps getting a fresh channel.
    pub evict_channel: bool,
}

/// Tracks, per HID node, the last completed inventory and how many
/// consecutive probes have failed since.
pub(crate) struct NodeLedger<K> {
    last_good: HashMap<K, DeviceInventory>,
    failures: HashMap<K, u8>,
}

// Hand-written: `derive(Default)` would needlessly bound `K: Default`, which
// `async_hid::DeviceId` doesn't (and needn't) satisfy.
impl<K> Default for NodeLedger<K> {
    fn default() -> Self {
        Self {
            last_good: HashMap::new(),
            failures: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone> NodeLedger<K> {
    /// Fold one node's probe result into the ledger and decide what to report.
    ///
    /// `healthy` means the node actually answered this tick — a completed
    /// receiver walk or a recognised/rejected direct probe — so `live` is
    /// authoritative (including `None` for "not one of ours"). An unhealthy
    /// tick means "couldn't check": the last-good inventory is replayed for up
    /// to [`NODE_MISS_GRACE`] consecutive failures, after which the live
    /// (partial or empty) result is surfaced.
    pub fn settle(
        &mut self,
        node: &K,
        healthy: bool,
        live: Option<DeviceInventory>,
    ) -> SettledNode {
        if healthy {
            self.failures.remove(node);
            let inventory = if let Some(inv) = live {
                self.last_good.insert(node.clone(), inv.clone());
                Some(inv)
            } else {
                self.last_good.remove(node);
                None
            };
            return SettledNode {
                inventory,
                evict_channel: false,
            };
        }

        let failures = self.failures.entry(node.clone()).or_insert(0);
        *failures = failures.saturating_add(1);
        let failures = *failures;
        let inventory = match self.last_good.get(node) {
            Some(prev) if failures <= NODE_MISS_GRACE => {
                debug!(
                    failures,
                    "node probe failed — replaying its last good inventory"
                );
                Some(prev.clone())
            }
            _ => {
                if self.last_good.remove(node).is_some() {
                    warn!(
                        failures,
                        "node probe failures exhausted the replay grace — surfacing the live result"
                    );
                }
                live
            }
        };
        SettledNode {
            inventory,
            evict_channel: failures >= CHANNEL_EVICT_AFTER,
        }
    }

    /// Drop ledger state for nodes the OS no longer enumerates — a vanished
    /// node is a real disconnect, so there is nothing to replay or heal.
    pub fn retain_nodes(&mut self, seen: &HashSet<K>) {
        self.last_good.retain(|node, _| seen.contains(node));
        self.failures.retain(|node, _| seen.contains(node));
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceInventory, ReceiverInfo};

    use super::{CHANNEL_EVICT_AFTER, NODE_MISS_GRACE, NodeLedger};

    fn inventory(name: &str) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: name.to_string(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: None,
            },
            paired: Vec::new(),
        }
    }

    fn receiver_name(inv: Option<&DeviceInventory>) -> Option<&str> {
        inv.map(|i| i.receiver.name.as_str())
    }

    #[test]
    fn failed_probe_replays_the_last_good_inventory_within_grace() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        for _ in 0..NODE_MISS_GRACE {
            let settled = ledger.settle(&1, false, None);
            assert_eq!(receiver_name(settled.inventory.as_ref()), Some("bolt"));
        }
    }

    #[test]
    fn replay_grace_expires_to_the_live_result() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        for _ in 0..NODE_MISS_GRACE {
            ledger.settle(&1, false, None);
        }
        // One failure past the grace: the (partial) live result wins, and the
        // exhausted snapshot is not resurrected by the following failure.
        let expired = ledger.settle(&1, false, Some(inventory("partial")));
        assert_eq!(receiver_name(expired.inventory.as_ref()), Some("partial"));
        let after = ledger.settle(&1, false, None);
        assert!(after.inventory.is_none());
    }

    #[test]
    fn a_healthy_tick_resets_the_failure_count() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        for _ in 0..NODE_MISS_GRACE {
            ledger.settle(&1, false, None);
        }
        ledger.settle(&1, true, Some(inventory("bolt")));
        let settled = ledger.settle(&1, false, None);
        assert_eq!(
            receiver_name(settled.inventory.as_ref()),
            Some("bolt"),
            "the recovery should re-arm the full replay grace"
        );
    }

    #[test]
    fn persistent_failure_keeps_requesting_channel_eviction() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        for i in 1..=NODE_MISS_GRACE + 2 {
            let settled = ledger.settle(&1, false, None);
            assert_eq!(
                settled.evict_channel,
                i >= CHANNEL_EVICT_AFTER,
                "tick {i}: eviction starts at the threshold and keeps firing"
            );
        }
        let recovered = ledger.settle(&1, true, Some(inventory("bolt")));
        assert!(!recovered.evict_channel);
    }

    #[test]
    fn a_healthy_empty_result_clears_the_replay_state() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        // The node answered and is genuinely not ours any more (e.g. a probe
        // that now rejects it): nothing must be replayed on a later failure.
        ledger.settle(&1, true, None);
        let settled = ledger.settle(&1, false, None);
        assert!(settled.inventory.is_none());
    }

    #[test]
    fn vanished_nodes_are_dropped_from_the_ledger() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("kept")));
        ledger.settle(&2, true, Some(inventory("gone")));
        ledger.retain_nodes(&std::iter::once(1).collect());
        let replayed = ledger.settle(&1, false, None);
        assert_eq!(receiver_name(replayed.inventory.as_ref()), Some("kept"));
        let dropped = ledger.settle(&2, false, None);
        assert!(
            dropped.inventory.is_none(),
            "a reappeared node starts clean"
        );
    }
}
