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

/// Cap on how many times a node's eviction threshold doubles while the
/// channels opened to replace a failed one keep failing too.
///
/// A replacement that fails the way its predecessor did is evidence that the
/// channel was not the problem: the OS can keep listing a node it will not let
/// this process talk to. While another process holds Secure Event Input,
/// macOS refuses report transfers to a keyboard-class device, which includes
/// a Bluetooth-direct keyboard's HID++ collection (`kIOReturnNotPermitted`,
/// #1799). loginwindow holds it whenever the display is off or the screen is
/// locked, which is hours; the very same handle answers again the moment it
/// is released. Reopening on every threshold instead churned a fresh OS
/// handle every few seconds for as long as it was held (#2617).
///
/// So every channel this ledger has evicted for failing doubles the threshold
/// its replacement is held to: the first replacement already needs twice the
/// base. The cap is `2^5` times the base, which at the recovery-scan cadence
/// still replaces a channel that truly needs it every several minutes (longer
/// on the arrival-replay path, whose base is higher). The first healthy probe
/// clears the backoff.
const MAX_REPLACEMENT_BACKOFF_DOUBLINGS: u8 = 5;

// `evict_threshold` shifts and multiplies in `u8`: the largest capped
// threshold must still fit, or it would saturate to one no count can reach.
const _: () = assert!(
    (ARRIVAL_REPLAY_EVICT_AFTER as u16) << MAX_REPLACEMENT_BACKOFF_DOUBLINGS <= u8::MAX as u16
        && CHANNEL_EVICT_AFTER as u16 <= ARRIVAL_REPLAY_EVICT_AFTER as u16
);

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
    /// Non-authoritative ticks since the last complete inventory.
    publication_misses: u8,
    /// Where the node's channel is in its eviction lifecycle. Changed only
    /// through [`Self::step`].
    channel: ChannelPhase,
}

impl FailureCounts {
    /// Apply one channel lifecycle event; the only writer of `channel`.
    /// Returns the failure count a `Failed` event reached on a serving
    /// channel.
    fn step(&mut self, event: ChannelEvent) -> Option<u8> {
        let (next, failures) = self.channel.step(event);
        self.channel = next;
        failures
    }
}

/// The node's channel, as far as eviction accounting goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelPhase {
    /// A channel serves the node, or none has been opened yet. Its failures
    /// count toward evicting it, against a threshold raised by `backoff`.
    Serving {
        /// Consecutive probes that produced no liveness evidence.
        probe: u8,
        /// Failed arrival replays on this channel.
        arrival_replay: u8,
        /// Consecutive channels evicted for failing, without a healthy probe
        /// in between; see [`MAX_REPLACEMENT_BACKOFF_DOUBLINGS`].
        backoff: u8,
    },
    /// The enumerator retired the serving channel at this ledger's request,
    /// and the node waits for a replacement. Nothing serves, so failed opens
    /// and passes over the draining channel count for nothing, and the
    /// channel cannot be retired a second time.
    AwaitingReplacement { backoff: u8 },
}

impl Default for ChannelPhase {
    fn default() -> Self {
        Self::Serving {
            probe: 0,
            arrival_replay: 0,
            backoff: 0,
        }
    }
}

/// What can happen to a node's channel, as the ledger sees it.
#[derive(Clone, Copy, Debug)]
enum ChannelEvent {
    /// One check of this kind failed.
    Failed(FailureKind),
    /// The enumerator retired the serving channel at this ledger's request.
    Retired,
    /// The enumerator opened a channel for the node.
    Opened,
}

impl ChannelPhase {
    /// The one transition function. Returns the next phase and, for a
    /// `Failed` event on a serving channel, the failure count it reached.
    const fn step(self, event: ChannelEvent) -> (Self, Option<u8>) {
        match (self, event) {
            (
                Self::Serving {
                    probe,
                    arrival_replay,
                    backoff,
                },
                ChannelEvent::Failed(FailureKind::Probe),
            ) => {
                let probe = probe.saturating_add(1);
                let next = Self::Serving {
                    probe,
                    arrival_replay,
                    backoff,
                };
                (next, Some(probe))
            }
            (
                Self::Serving {
                    arrival_replay,
                    backoff,
                    ..
                },
                ChannelEvent::Failed(FailureKind::ArrivalReplay),
            ) => {
                // The pairing-count response proves transport liveness, so a
                // later ordinary failure must start its own streak again.
                let arrival_replay = arrival_replay.saturating_add(1);
                let next = Self::Serving {
                    probe: 0,
                    arrival_replay,
                    backoff,
                };
                (next, Some(arrival_replay))
            }
            (Self::Serving { backoff, .. }, ChannelEvent::Retired) => {
                (Self::AwaitingReplacement { backoff }, None)
            }
            // A channel opened without a failed predecessor (first sight, a
            // node back from absence) is not evidence against the channel.
            (Self::Serving { backoff, .. }, ChannelEvent::Opened) => (
                Self::Serving {
                    probe: 0,
                    arrival_replay: 0,
                    backoff,
                },
                None,
            ),
            (Self::AwaitingReplacement { backoff }, ChannelEvent::Opened) => (
                Self::Serving {
                    probe: 0,
                    arrival_replay: 0,
                    backoff: backoff.saturating_add(1),
                },
                None,
            ),
            (
                state @ Self::AwaitingReplacement { .. },
                ChannelEvent::Failed(_) | ChannelEvent::Retired,
            ) => (state, None),
        }
    }

    /// Consecutive channels evicted for failing, in either phase.
    const fn backoff(self) -> u8 {
        match self {
            Self::Serving { backoff, .. } | Self::AwaitingReplacement { backoff } => backoff,
        }
    }

    /// The failure count at which a serving channel is evicted, for a
    /// failure kind whose unscaled threshold is `base`.
    fn evict_threshold(self, base: u8) -> u8 {
        let doublings = self.backoff().min(MAX_REPLACEMENT_BACKOFF_DOUBLINGS);
        base.saturating_mul(1 << doublings)
    }
}

#[derive(Clone, Copy, Debug)]
enum FailureKind {
    Probe,
    ArrivalReplay,
}

impl FailureKind {
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
    /// replacement channel, without re-arming expired inventory replay. A
    /// replacement for a channel evicted for failing raises the threshold the
    /// new one is held to; only a healthy probe lowers it again.
    pub fn reset_channel_failures_after_open(&mut self, node: &K) {
        if let Some(counts) = self.failures.get_mut(node) {
            counts.step(ChannelEvent::Opened);
        }
    }

    /// Record that the enumerator retired the node's serving channel on this
    /// ledger's [`SettledNode::evict_channel`] request, so the channel opened
    /// in its place is held to a raised threshold.
    pub fn record_eviction(&mut self, node: &K) {
        if let Some(counts) = self.failures.get_mut(node) {
            counts.step(ChannelEvent::Retired);
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
        let channel_failures = counts.step(ChannelEvent::Failed(failure_kind));
        counts.publication_misses = counts.publication_misses.saturating_add(1);
        let evict_channel =
            channel_failures.is_some_and(|n| n >= counts.channel.evict_threshold(evict_after));
        let failed_replacements = counts.channel.backoff();
        let publication_misses = counts.publication_misses;
        let failure_kind = failure_kind.label();
        let inventory = if publication_misses <= NODE_MISS_GRACE {
            if let Some(previous) = self.last_good.get(node) {
                debug!(
                    failures = publication_misses,
                    ?channel_failures,
                    failed_replacements,
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
                    ?channel_failures,
                    failed_replacements,
                    failure_kind,
                    "node check failures exhausted the replay grace — surfacing the live result"
                );
            }
            live
        };
        SettledNode {
            inventory,
            evict_channel,
        }
    }

    /// Fold in a tick that never asked the node — another OpenLogi process
    /// held its receiver register phase — and so is evidence of nothing: the
    /// last-good inventory is replayed, the failure count is left alone, and
    /// no eviction is requested. A channel that was not used cannot have
    /// failed, and retiring it would tear down every device behind it for a
    /// contention that is not its own.
    pub fn defer(&self, node: &K) -> SettledNode {
        SettledNode {
            inventory: self.last_good.get(node).cloned(),
            evict_channel: false,
        }
    }

    /// Drop ledger state for nodes the OS no longer enumerates — a vanished
    /// node is a real disconnect, so there is nothing to replay or heal.
    pub fn retain_nodes(&mut self, seen: &HashSet<K>) {
        self.last_good.retain(|node, _| seen.contains(node));
        self.failures.retain(|node, _| seen.contains(node));
    }

    /// Whether the node's last complete inventory is still served — live, or
    /// replayed within its grace. `false` once the grace expired and the live
    /// result surfaced instead.
    pub(super) fn contains_last_good(&self, node: &K) -> bool {
        self.last_good.contains_key(node)
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceInventory, ReceiverInfo};

    use super::{
        ARRIVAL_REPLAY_EVICT_AFTER, CHANNEL_EVICT_AFTER, ChannelEvent, ChannelPhase, FailureKind,
        MAX_REPLACEMENT_BACKOFF_DOUBLINGS, NODE_MISS_GRACE, NodeLedger,
    };

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
            loop {
                let settled = ledger.settle(&1, false, None);
                published.push(settled.inventory.is_some());
                if settled.evict_channel {
                    ledger.record_eviction(&1);
                    break;
                }
            }
        }

        let replayed = published.iter().filter(|&&shown| shown).count();
        assert_eq!(
            replayed,
            usize::from(NODE_MISS_GRACE),
            "replacement channels must not re-arm the replay grace"
        );
        assert!(published[..replayed].iter().all(|&shown| shown));
    }

    /// Failed probes it takes to evict the node's current channel, after the
    /// enumerator opens a replacement; the eviction is carried out, as the
    /// enumerator does when a channel was serving.
    fn failures_until_eviction(ledger: &mut NodeLedger<i32>) -> u16 {
        ledger.reset_channel_failures_after_open(&1);
        for failures in 1..=u16::from(u8::MAX) {
            if ledger.settle(&1, false, None).evict_channel {
                ledger.record_eviction(&1);
                return failures;
            }
        }
        panic!("the channel was never evicted");
    }

    /// A channel opened to replace a failed one, which fails the same way, is
    /// held to twice the previous threshold: a node the OS keeps listing but
    /// that stops answering (a Bluetooth keyboard while the display is off)
    /// is not reopened every few seconds for hours.
    /// The backoff is capped so a node that does need a fresh channel gets one.
    #[test]
    fn replacements_that_keep_failing_back_off_eviction() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));

        let thresholds: Vec<u16> = (0..8)
            .map(|_| failures_until_eviction(&mut ledger))
            .collect();

        let base = u16::from(CHANNEL_EVICT_AFTER);
        let cap = base << MAX_REPLACEMENT_BACKOFF_DOUBLINGS;
        assert_eq!(
            thresholds,
            vec![base, base * 2, base * 4, base * 8, base * 16, cap, cap, cap]
        );
    }

    #[test]
    fn a_healthy_probe_clears_the_replacement_backoff() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));
        for _ in 0..3 {
            failures_until_eviction(&mut ledger);
        }

        ledger.settle(&1, true, Some(inventory("craft")));

        assert_eq!(
            failures_until_eviction(&mut ledger),
            u16::from(CHANNEL_EVICT_AFTER),
            "a node that answered again starts from the base threshold"
        );
    }

    /// Opening a channel that did not replace a failed one (first sight, or a
    /// node back from absence) is not evidence against the channel.
    #[test]
    fn an_open_without_a_failed_predecessor_keeps_the_base_threshold() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));
        assert!(!ledger.settle(&1, false, None).evict_channel);

        ledger.reset_channel_failures_after_open(&1);
        ledger.reset_channel_failures_after_open(&1);

        assert_eq!(
            failures_until_eviction(&mut ledger),
            u16::from(CHANNEL_EVICT_AFTER)
        );
    }

    /// Failed opens and a retiring channel's passes settle as failures past
    /// the threshold without any channel being retired: the channel that
    /// finally opens did not replace a failed one.
    #[test]
    fn failures_with_no_channel_to_retire_do_not_back_off() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));
        for _ in 0..CHANNEL_EVICT_AFTER * 4 {
            ledger.settle(&1, false, None);
        }

        assert_eq!(
            failures_until_eviction(&mut ledger),
            u16::from(CHANNEL_EVICT_AFTER)
        );
    }

    /// Every phase against every event: the whole transition function.
    #[test]
    fn channel_phase_transitions() {
        use ChannelEvent::{Failed, Opened, Retired};
        use ChannelPhase::{AwaitingReplacement, Serving};
        use FailureKind::{ArrivalReplay, Probe};

        let serving = Serving {
            probe: 1,
            arrival_replay: 2,
            backoff: 3,
        };
        let awaiting = AwaitingReplacement { backoff: 3 };
        let fresh = |backoff| Serving {
            probe: 0,
            arrival_replay: 0,
            backoff,
        };
        let table = [
            (
                serving,
                Failed(Probe),
                Serving {
                    probe: 2,
                    arrival_replay: 2,
                    backoff: 3,
                },
                Some(2),
            ),
            (
                serving,
                Failed(ArrivalReplay),
                Serving {
                    probe: 0,
                    arrival_replay: 3,
                    backoff: 3,
                },
                Some(3),
            ),
            (serving, Retired, awaiting, None),
            (serving, Opened, fresh(3), None),
            (awaiting, Failed(Probe), awaiting, None),
            (awaiting, Failed(ArrivalReplay), awaiting, None),
            (awaiting, Retired, awaiting, None),
            (awaiting, Opened, fresh(4), None),
        ];
        for (from, event, to, failures) in table {
            assert_eq!(from.step(event), (to, failures), "{from:?} on {event:?}");
        }
    }

    /// A channel the enumerator reports retired twice is one retirement.
    #[test]
    fn a_duplicate_retirement_is_refused() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));
        while !ledger.settle(&1, false, None).evict_channel {}
        ledger.record_eviction(&1);
        ledger.record_eviction(&1);

        assert_eq!(
            failures_until_eviction(&mut ledger),
            u16::from(CHANNEL_EVICT_AFTER) * 2,
            "one retirement doubles the threshold once"
        );
    }

    /// Opening twice after one retirement replaces the failed channel once.
    #[test]
    fn a_duplicate_replacement_open_is_refused() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));
        while !ledger.settle(&1, false, None).evict_channel {}
        ledger.record_eviction(&1);
        ledger.reset_channel_failures_after_open(&1);

        assert_eq!(
            failures_until_eviction(&mut ledger),
            u16::from(CHANNEL_EVICT_AFTER) * 2,
            "the second open does not count as another replacement"
        );
    }

    /// Nothing serves while the node waits for its replacement, so failed
    /// opens and draining passes never ask for an eviction.
    #[test]
    fn failures_while_awaiting_a_replacement_request_no_eviction() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("craft")));
        while !ledger.settle(&1, false, None).evict_channel {}
        ledger.record_eviction(&1);

        for _ in 0..u16::from(CHANNEL_EVICT_AFTER) * 8 {
            assert!(!ledger.settle(&1, false, None).evict_channel);
            assert!(!ledger.settle_arrival_replay_failure(&1).evict_channel);
        }
    }

    /// The arrival-replay path backs off from its own, longer base.
    #[test]
    fn arrival_replay_replacements_back_off_from_their_own_base() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("unifying")));
        for _ in 1..ARRIVAL_REPLAY_EVICT_AFTER {
            ledger.settle_arrival_replay_failure(&1);
        }
        assert!(ledger.settle_arrival_replay_failure(&1).evict_channel);
        ledger.record_eviction(&1);

        ledger.reset_channel_failures_after_open(&1);
        let mut failures = 0;
        while !ledger.settle_arrival_replay_failure(&1).evict_channel {
            failures += 1;
        }

        assert_eq!(failures + 1, ARRIVAL_REPLAY_EVICT_AFTER * 2);
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

    /// Contention on the receiver's register phase is not the channel's
    /// fault: however many ticks defer to the other process, the replay stays
    /// within grace and the eviction count does not move — the next real
    /// failure continues from where the count was.
    #[test]
    fn deferred_ticks_replay_without_counting_toward_eviction() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        assert!(!ledger.settle(&1, false, None).evict_channel);

        for _ in 0..NODE_MISS_GRACE + 2 {
            let deferred = ledger.defer(&1);
            assert!(!deferred.evict_channel, "a deferred tick never evicts");
            assert_eq!(receiver_name(deferred.inventory.as_ref()), Some("bolt"));
        }

        assert!(
            ledger.settle(&1, false, None).evict_channel,
            "the second real failure still evicts: deferrals left the count at one"
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
