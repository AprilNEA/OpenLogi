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
//! Generic over the node key ([`crate::backend::NodeId`] in production) purely
//! so the decision table can be exercised with a trivial key, keeping the
//! tests about the replay policy rather than about node identity.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use openlogi_core::device::DeviceInventory;
use tracing::{debug, warn};

/// Ticks a node's last-good inventory keeps being served while its probe does
/// not produce an authoritative result. Opening a replacement channel does not
/// reset this clock: a receiver that is genuinely wedged must eventually show
/// the truth instead of repeatedly resurrecting an ever-staler snapshot.
/// Mirrors the probe cache's `CACHE_MISS_GRACE`, so a node recovers with its
/// memoized probes still warm.
const NODE_MISS_GRACE: u8 = 3;

/// Consecutive failed probes after which the node's cached channel should be
/// dropped and reopened. A channel whose read loop parked on a `Disconnected`
/// handle never recovers on its own — the transport contract (see
/// `AsyncHidChannel::read_report`) expects the inventory watcher to evict it,
/// and node-vanish eviction never fires for a node the OS keeps listing.
const CHANNEL_EVICT_AFTER: u8 = 2;

/// Consecutive arrival-replay failures tolerated after the receiver answered
/// its pairing-count register. This path gets a longer channel grace because
/// it proves the transport is responsive, but cannot be trusted forever
/// because the published device list may be stale.
const ARRIVAL_REPLAY_EVICT_AFTER: u8 = NODE_MISS_GRACE + 1;

/// What [`NodeLedger::settle`] decided for one node this tick.
pub(crate) struct SettledNode {
    /// The inventory to report for the node: the live result, or the replayed
    /// last-good snapshot while the failure is within grace.
    pub inventory: Option<DeviceInventory>,
    /// Whether the node's cached channel should be dropped so the next tick
    /// reopens it. `true` once the current failure kind reaches its threshold,
    /// so a persistently sick node keeps getting a fresh channel.
    pub evict_channel: bool,
}

/// Tracks each HID node's last completed inventory, bounded replay age, and
/// channel-local failure streaks.
pub(crate) struct NodeLedger<K> {
    last_good: HashMap<K, DeviceInventory>,
    failures: HashMap<K, FailureCounts>,
}

#[derive(Default)]
struct FailureCounts {
    /// Consecutive probes that produced no liveness evidence.
    probe: u8,
    /// Failed arrival replays on the current channel generation.
    arrival_replay: u8,
    /// Non-authoritative ticks since the last complete inventory.
    publication_misses: u8,
}

#[derive(Clone, Copy)]
enum FailureKind {
    Probe,
    ArrivalReplay,
}

impl FailureKind {
    fn record(self, counts: &mut FailureCounts) -> u8 {
        match self {
            Self::Probe => {
                counts.probe = counts.probe.saturating_add(1);
                counts.probe
            }
            Self::ArrivalReplay => {
                // The pairing-count response proves transport liveness, so a
                // later ordinary failure must start its own streak again.
                counts.probe = 0;
                counts.arrival_replay = counts.arrival_replay.saturating_add(1);
                counts.arrival_replay
            }
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::ArrivalReplay => "arrival replay",
        }
    }
}

// Hand-written: `derive(Default)` would needlessly bound `K: Default`, which
// a node key doesn't (and needn't) satisfy.
impl<K> Default for NodeLedger<K> {
    fn default() -> Self {
        Self {
            last_good: HashMap::new(),
            failures: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone> NodeLedger<K> {
    /// Start channel-eviction accounting again after the enumerator opens a
    /// replacement channel, without re-arming expired inventory replay.
    pub fn reset_channel_failures_after_open(&mut self, node: &K) {
        if let Some(counts) = self.failures.get_mut(node) {
            counts.probe = 0;
            counts.arrival_replay = 0;
        }
    }

    /// Keep the last complete inventory briefly after a receiver proved it can
    /// still answer but could not replay its arrival notifications this tick.
    ///
    /// This is distinct from [`Self::settle`] with `healthy = false`: an
    /// arrival-trigger write may fail transiently while ordinary receiver
    /// registers and an already-armed capture session remain functional.
    pub fn settle_arrival_replay_failure(&mut self, node: &K) -> SettledNode {
        self.settle_failure(
            node,
            None,
            ARRIVAL_REPLAY_EVICT_AFTER,
            FailureKind::ArrivalReplay,
        )
    }

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

        self.settle_failure(node, live, CHANNEL_EVICT_AFTER, FailureKind::Probe)
    }

    fn settle_failure(
        &mut self,
        node: &K,
        live: Option<DeviceInventory>,
        evict_after: u8,
        failure_kind: FailureKind,
    ) -> SettledNode {
        let counts = self.failures.entry(node.clone()).or_default();
        let channel_failures = failure_kind.record(counts);
        counts.publication_misses = counts.publication_misses.saturating_add(1);
        let publication_misses = counts.publication_misses;
        let failure_kind = failure_kind.label();
        let inventory = if publication_misses <= NODE_MISS_GRACE {
            if let Some(previous) = self.last_good.get(node) {
                debug!(
                    failures = publication_misses,
                    channel_failures,
                    failure_kind,
                    "node check incomplete — replaying its last good inventory"
                );
                Some(previous.clone())
            } else {
                live
            }
        } else {
            if self.last_good.remove(node).is_some() {
                warn!(
                    failures = publication_misses,
                    channel_failures,
                    failure_kind,
                    "node check failures exhausted the replay grace — surfacing the live result"
                );
            }
            live
        };
        SettledNode {
            inventory,
            evict_channel: channel_failures >= evict_after,
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

    use super::{ARRIVAL_REPLAY_EVICT_AFTER, CHANNEL_EVICT_AFTER, NODE_MISS_GRACE, NodeLedger};

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
    fn transient_arrival_replay_failures_keep_the_channel_and_snapshot() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 1..ARRIVAL_REPLAY_EVICT_AFTER {
            let retained = ledger.settle_arrival_replay_failure(&1);
            assert_eq!(receiver_name(retained.inventory.as_ref()), Some("unifying"));
            assert!(!retained.evict_channel);
        }
    }

    #[test]
    fn persistent_arrival_replay_failure_retires_the_channel_and_snapshot() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 1..ARRIVAL_REPLAY_EVICT_AFTER {
            ledger.settle_arrival_replay_failure(&1);
        }

        let exhausted = ledger.settle_arrival_replay_failure(&1);

        assert!(exhausted.evict_channel);
        assert!(exhausted.inventory.is_none());
    }

    #[test]
    fn expired_arrival_snapshot_does_not_resurface_on_probe_failure() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 0..ARRIVAL_REPLAY_EVICT_AFTER {
            ledger.settle_arrival_replay_failure(&1);
        }

        let retiring = ledger.settle(&1, false, None);

        assert!(retiring.inventory.is_none());
        assert!(!retiring.evict_channel);
    }

    #[test]
    fn complete_probe_resets_the_arrival_replay_failure_streak() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 1..ARRIVAL_REPLAY_EVICT_AFTER {
            ledger.settle_arrival_replay_failure(&1);
        }
        ledger.settle(&1, true, Some(inventory("unifying")));

        let retained = ledger.settle_arrival_replay_failure(&1);

        assert!(!retained.evict_channel);
        assert_eq!(receiver_name(retained.inventory.as_ref()), Some("unifying"));
    }

    #[test]
    fn one_probe_timeout_does_not_inherit_arrival_replay_failures() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        ledger.settle_arrival_replay_failure(&1);
        ledger.settle_arrival_replay_failure(&1);

        let first_probe_timeout = ledger.settle(&1, false, None);

        assert!(!first_probe_timeout.evict_channel);
        assert_eq!(
            receiver_name(first_probe_timeout.inventory.as_ref()),
            Some("unifying")
        );
        assert!(
            ledger.settle(&1, false, None).evict_channel,
            "ordinary failures still retire on their own second consecutive tick"
        );
    }

    #[test]
    fn mixed_timeouts_do_not_hide_persistent_arrival_replay_failure() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 1..ARRIVAL_REPLAY_EVICT_AFTER {
            assert!(!ledger.settle_arrival_replay_failure(&1).evict_channel);
            assert!(!ledger.settle(&1, false, None).evict_channel);
        }

        let exhausted = ledger.settle_arrival_replay_failure(&1);

        assert!(exhausted.evict_channel);
        assert!(exhausted.inventory.is_none());
    }

    #[test]
    fn replacement_channel_gets_fresh_eviction_grace_without_resurrecting_inventory() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 0..ARRIVAL_REPLAY_EVICT_AFTER - 1 {
            ledger.settle_arrival_replay_failure(&1);
        }
        assert!(!ledger.settle(&1, false, None).evict_channel);
        let retired = ledger.settle(&1, false, None);
        assert!(retired.evict_channel);
        assert!(retired.inventory.is_none());

        for _ in 0..=NODE_MISS_GRACE {
            ledger.settle(&1, false, None);
        }
        assert!(
            ledger.settle(&1, false, None).inventory.is_none(),
            "a long retirement must still stop publishing stale inventory"
        );

        ledger.reset_channel_failures_after_open(&1);
        let replacement_failure = ledger.settle_arrival_replay_failure(&1);

        assert!(!replacement_failure.evict_channel);
        assert!(replacement_failure.inventory.is_none());
    }

    #[test]
    fn repeated_channel_opens_do_not_rearm_inventory_replay() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        let mut published = Vec::new();

        for _ in 0..3 {
            ledger.reset_channel_failures_after_open(&1);
            let first = ledger.settle(&1, false, None);
            published.push(first.inventory.is_some());
            assert!(!first.evict_channel);

            let second = ledger.settle(&1, false, None);
            published.push(second.inventory.is_some());
            assert!(second.evict_channel);
        }

        assert_eq!(published, vec![true, true, true, false, false, false]);
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

    /// A receiver probe that burns its whole budget arrives here as one
    /// unhealthy tick. It must not evict on its own: the budget leaves barely a
    /// second over its documented worst case, so one lost reply during a
    /// legitimate deep walk lands here too — and evicting unpublishes every
    /// device behind that receiver. The devices stay visible via the replay
    /// while a channel that really is dead trips the threshold next tick.
    #[test]
    fn one_failed_probe_never_evicts_but_keeps_the_devices_visible() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));

        let first = ledger.settle(&1, false, None);

        assert!(!first.evict_channel);
        assert_eq!(receiver_name(first.inventory.as_ref()), Some("bolt"));
        assert!(
            ledger.settle(&1, false, None).evict_channel,
            "a node that keeps failing is still replaced on the next tick"
        );
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
