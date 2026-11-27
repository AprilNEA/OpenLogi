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
//! channel so the next tick reopens it fresh. Replacements that stay just as
//! unanswered need progressively more failures before they are retired in
//! turn, until the node answers again.
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
/// dropped and reopened — the base threshold, before any
/// [`EVICT_BACKOFF_MAX_DOUBLINGS`] backoff. A channel whose read loop parked on a `Disconnected`
/// handle never recovers on its own — the transport contract (see
/// `AsyncHidChannel::read_report`) expects the inventory watcher to evict it,
/// and node-vanish eviction never fires for a node the OS keeps listing.
const CHANNEL_EVICT_AFTER: u8 = 2;

/// Cap on how often [`CHANNEL_EVICT_AFTER`] doubles for replacement channels
/// that stay unanswered. A Bluetooth-direct device asleep on a live link keeps
/// its node listed and answers no channel, so reopening at the base threshold
/// retires a channel every ~35 s for as long as it sleeps, and every
/// retire/reopen cycle has measured at ~0.5 MB of agent memory never returned
/// (#735). At the cap a sick node still gets a fresh channel every 32 failed
/// probes instead of every 2.
const EVICT_BACKOFF_MAX_DOUBLINGS: u8 = 4;

const _: () = assert!(
    (CHANNEL_EVICT_AFTER as u16) << EVICT_BACKOFF_MAX_DOUBLINGS <= u8::MAX as u16,
    "the backed-off eviction threshold must fit the u8 failure count"
);

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
    /// so a persistently sick node keeps getting a fresh channel — at a
    /// backed-off cadence while each replacement stays unanswered.
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
    /// Where the node's current channel stands in its probe lifecycle.
    channel: ChannelHealth,
    /// Failed arrival replays on the current channel generation.
    arrival_replay: u8,
    /// Non-authoritative ticks since the last complete inventory.
    publication_misses: u8,
}

/// The probe lifecycle of a node's current channel since the node last showed
/// liveness. [`Self::transition`] is the only place it changes; a healthy
/// [`NodeLedger::settle`] drops it with the rest of the node's failure state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelHealth {
    /// The current channel is in service. `failures` counts its consecutive
    /// failed probes; `doublings` counts the channels before it that were
    /// retired unanswered, each doubling the threshold that retires this one.
    Probing { failures: u8, doublings: u8 },
    /// The current channel reached its threshold and awaits replacement.
    Retired {
        failures: u8,
        doublings: u8,
        reason: RetireReason,
    },
}

/// Why a channel was retired, which decides whether its replacement backs off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetireReason {
    /// Nothing answered the channel: the replacement backs off.
    Unanswered,
    /// The node answered but its walk stayed incomplete: no backoff.
    Incomplete,
}

/// What one tick or open told the ledger about the node's current channel.
#[derive(Clone, Copy)]
enum ChannelEvent {
    /// A probe got no answer.
    Unanswered,
    /// The node answered its liveness register, but the walk stayed
    /// incomplete. Still a failed probe, at the base threshold: the streak
    /// keeps the channel's earlier unanswered failures, so a channel that was
    /// silent and now answers only partially is retired promptly.
    AnsweredIncomplete,
    /// A receiver answered its liveness register on the arrival-replay path;
    /// not a failed probe.
    Alive,
    /// A replacement channel opened.
    Reopened,
}

impl Default for ChannelHealth {
    fn default() -> Self {
        Self::Probing {
            failures: 0,
            doublings: 0,
        }
    }
}

impl ChannelHealth {
    fn transition(self, event: ChannelEvent) -> Self {
        let (failures, doublings) = self.counts();
        match event {
            ChannelEvent::Alive => Self::default(),
            ChannelEvent::Reopened => Self::Probing {
                failures: 0,
                doublings: match self {
                    Self::Retired {
                        reason: RetireReason::Unanswered,
                        ..
                    } => doublings.saturating_add(1).min(EVICT_BACKOFF_MAX_DOUBLINGS),
                    Self::Retired { .. } | Self::Probing { .. } => doublings,
                },
            },
            ChannelEvent::Unanswered => Self::failed(failures, doublings, RetireReason::Unanswered),
            ChannelEvent::AnsweredIncomplete => Self::failed(failures, 0, RetireReason::Incomplete),
        }
    }

    /// One more failed probe, retiring the channel once it reaches the
    /// threshold for `doublings`.
    fn failed(failures: u8, doublings: u8, reason: RetireReason) -> Self {
        let failures = failures.saturating_add(1);
        if failures >= CHANNEL_EVICT_AFTER << doublings.min(EVICT_BACKOFF_MAX_DOUBLINGS) {
            Self::Retired {
                failures,
                doublings,
                reason,
            }
        } else {
            Self::Probing {
                failures,
                doublings,
            }
        }
    }

    const fn counts(self) -> (u8, u8) {
        match self {
            Self::Probing {
                failures,
                doublings,
            }
            | Self::Retired {
                failures,
                doublings,
                ..
            } => (failures, doublings),
        }
    }

    const fn is_retired(self) -> bool {
        matches!(self, Self::Retired { .. })
    }
}

/// One non-authoritative tick, by what it says about the node's channel.
#[derive(Clone, Copy)]
enum Tick {
    /// The current channel was probed.
    Probe(ChannelEvent),
    /// A receiver answered but could not replay its arrival notifications.
    ArrivalReplay,
    /// No channel served the node: it would not open, or its previous one is
    /// still draining. Nothing was probed, so no channel failed.
    NoChannel,
}

impl Tick {
    /// Fold the tick into the channel state; returns the streak it extended
    /// and whether the channel must be retired.
    fn record(self, counts: &mut FailureCounts) -> (u8, bool) {
        match self {
            Self::Probe(event) => {
                counts.channel = counts.channel.transition(event);
                (counts.channel.counts().0, counts.channel.is_retired())
            }
            Self::ArrivalReplay => {
                // The pairing-count response proves transport liveness, so a
                // later ordinary failure must start its own streak again, at
                // the base threshold.
                counts.channel = counts.channel.transition(ChannelEvent::Alive);
                counts.arrival_replay = counts.arrival_replay.saturating_add(1);
                (
                    counts.arrival_replay,
                    counts.arrival_replay >= ARRIVAL_REPLAY_EVICT_AFTER,
                )
            }
            Self::NoChannel => (counts.channel.counts().0, false),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Probe(ChannelEvent::AnsweredIncomplete) => "incomplete walk",
            Self::Probe(_) => "probe",
            Self::ArrivalReplay => "arrival replay",
            Self::NoChannel => "no channel",
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
    /// replacement for a channel retired unanswered backs off the next probe
    /// eviction until the node shows liveness again.
    pub fn reset_channel_failures_after_open(&mut self, node: &K) {
        if let Some(counts) = self.failures.get_mut(node) {
            counts.channel = counts.channel.transition(ChannelEvent::Reopened);
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
        self.settle_failure(node, None, Tick::ArrivalReplay)
    }

    /// Fold in a probe whose node answered its liveness register but left the
    /// walk incomplete (a Bolt slot that would not read). It counts toward
    /// retiring the channel like any failed probe, but at the base threshold:
    /// an answering node is not the unanswered case the backoff is for.
    pub fn settle_incomplete(&mut self, node: &K, live: Option<DeviceInventory>) -> SettledNode {
        self.settle_failure(node, live, Tick::Probe(ChannelEvent::AnsweredIncomplete))
    }

    /// Fold in a tick on which no channel served the node — it would not open,
    /// or its previous channel is still draining. The replay grace advances,
    /// but no channel failed, so no eviction or backoff is recorded.
    pub fn settle_without_channel(&mut self, node: &K) -> SettledNode {
        self.settle_failure(node, None, Tick::NoChannel)
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

        self.settle_failure(node, live, Tick::Probe(ChannelEvent::Unanswered))
    }

    fn settle_failure(
        &mut self,
        node: &K,
        live: Option<DeviceInventory>,
        tick: Tick,
    ) -> SettledNode {
        let counts = self.failures.entry(node.clone()).or_default();
        let (channel_failures, evict_channel) = tick.record(counts);
        counts.publication_misses = counts.publication_misses.saturating_add(1);
        let publication_misses = counts.publication_misses;
        let failure_kind = tick.label();
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
            loop {
                let settled = ledger.settle(&1, false, None);
                published.push(settled.inventory.is_some());
                if settled.evict_channel {
                    break;
                }
            }
        }

        let grace = usize::from(NODE_MISS_GRACE);
        assert!(published[..grace].iter().all(|&replayed| replayed));
        assert!(
            published[grace..].iter().all(|&replayed| !replayed),
            "replacement channels must not re-arm the expired replay: {published:?}"
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

    /// Failed probes on the current channel until the ledger asks to retire it.
    fn failures_until_eviction(ledger: &mut NodeLedger<i32>) -> u32 {
        (1..=u32::from(u8::MAX))
            .find(|_| ledger.settle(&1, false, None).evict_channel)
            .expect("a node that never answers is eventually retired")
    }

    /// A Bluetooth-direct device asleep on a live link: the OS keeps the node
    /// listed and every replacement channel stays as unanswered as the last.
    /// Each unanswered replacement doubles the failures needed before the
    /// next retirement, so the reopen cadence decays from ~35 s toward
    /// minutes instead of churning OS handles all night (#735).
    #[test]
    fn unanswered_replacements_back_off_channel_eviction() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("keyboard")));

        let mut thresholds = Vec::new();
        for _ in 0..7 {
            thresholds.push(failures_until_eviction(&mut ledger));
            ledger.reset_channel_failures_after_open(&1);
        }
        assert_eq!(thresholds, vec![2, 4, 8, 16, 32, 32, 32]);

        ledger.settle(&1, true, Some(inventory("keyboard")));
        assert_eq!(
            failures_until_eviction(&mut ledger),
            u32::from(CHANNEL_EVICT_AFTER),
            "an answered probe restores the prompt first retirement"
        );
    }

    /// A channel replaced before its probe threshold (a capture-reported dead
    /// channel) was not retired unanswered, and a tick with no channel at all
    /// failed nothing: neither backs off the next retirement.
    #[test]
    fn a_replacement_before_the_threshold_does_not_back_off() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("keyboard")));
        assert!(!ledger.settle(&1, false, None).evict_channel);
        ledger.reset_channel_failures_after_open(&1);
        for _ in 0..3 {
            assert!(!ledger.settle_without_channel(&1).evict_channel);
        }

        ledger.reset_channel_failures_after_open(&1);
        assert_eq!(
            failures_until_eviction(&mut ledger),
            u32::from(CHANNEL_EVICT_AFTER)
        );
    }

    /// The receiver answering its pairing-count register proves the link is
    /// alive, so it clears the backoff as an answered probe would.
    #[test]
    fn arrival_replay_liveness_clears_the_eviction_backoff() {
        let mut ledger = NodeLedger::default();
        ledger.settle(&1, true, Some(inventory("bolt")));
        failures_until_eviction(&mut ledger);
        ledger.reset_channel_failures_after_open(&1);
        failures_until_eviction(&mut ledger);
        ledger.reset_channel_failures_after_open(&1);

        ledger.settle_arrival_replay_failure(&1);
        assert_eq!(
            failures_until_eviction(&mut ledger),
            u32::from(CHANNEL_EVICT_AFTER)
        );
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
