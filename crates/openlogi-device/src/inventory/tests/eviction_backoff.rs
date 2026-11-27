//! Which ticks may grow the ledger's channel-eviction backoff: only a channel
//! that went unanswered, never a missing channel or a receiver that answered.

use super::*;
use crate::inventory::ledger::NodeLedger;

const NODE: i32 = 1;

/// Unanswered probes on the current channel until the ledger retires it.
fn failures_until_eviction(ledger: &mut NodeLedger<i32>) -> usize {
    (1..=usize::from(u8::MAX))
        .find(|_| ledger.settle(&NODE, false, None).evict_channel)
        .expect("a node that never answers is eventually retired")
}

/// Retire `n` consecutive replacement channels for unanswered probes.
fn back_off(ledger: &mut NodeLedger<i32>, n: usize) {
    for _ in 0..n {
        failures_until_eviction(ledger);
        ledger.reset_channel_failures_after_open(&NODE);
    }
}

fn seeded_ledger() -> NodeLedger<i32> {
    let mut ledger = NodeLedger::default();
    ledger.settle(&NODE, true, Some(inventory(&[1]).remove(0)));
    ledger
}

/// A node that will not open has no channel to retire, so failed opens before
/// the first successful one must leave the first channel's threshold alone.
#[test]
fn failed_opens_do_not_back_off_the_first_channel() {
    let mut ledger = seeded_ledger();
    let (mut complete, mut healthy) = (true, true);
    for _ in 0..2 {
        settle_unhealthy_node(&mut ledger, &NODE, &mut complete, &mut healthy);
    }

    ledger.reset_channel_failures_after_open(&NODE);
    assert_eq!(failures_until_eviction(&mut ledger), 2);
}

/// A Bolt receiver that answers its pairing count but leaves a slot
/// unreadable is alive: the walk is incomplete, the link is not unanswered.
/// Its answer clears any backoff built while it was silent.
#[test]
fn a_bolt_receiver_that_answers_partially_clears_the_backoff() {
    let mut ledger = seeded_ledger();
    back_off(&mut ledger, 2);

    let partial = assemble_bolt_probe(bolt_receiver_info(), Some(2), vec![bolt_slot(1)]);
    settle_probe(&mut ledger, &NODE, partial.verdict, partial.inventory);

    ledger.reset_channel_failures_after_open(&NODE);
    assert_eq!(failures_until_eviction(&mut ledger), 2);
}

/// An incomplete walk is still a failed probe: replayed within grace, and a
/// receiver that keeps leaving slots unread still gets a fresh channel at the
/// base threshold, as before the backoff existed (#218).
#[test]
fn incomplete_walks_still_retire_at_the_base_threshold() {
    let mut ledger = seeded_ledger();
    let known = inventory(&[1]).remove(0);
    let walk = || assemble_bolt_probe(bolt_receiver_info(), Some(2), vec![bolt_slot(1)]);

    let first = walk();
    let first = settle_probe(&mut ledger, &NODE, first.verdict, first.inventory);
    assert_eq!(first.inventory, Some(known));
    assert!(!first.evict_channel);

    let second = walk();
    let second = settle_probe(&mut ledger, &NODE, second.verdict, second.inventory);
    assert!(second.evict_channel);

    ledger.reset_channel_failures_after_open(&NODE);
    assert_eq!(
        failures_until_eviction(&mut ledger),
        2,
        "a channel retired for an incomplete walk was answered: no backoff"
    );
}

/// The ticks between an unanswered retirement and its replacement — the old
/// channel draining, the reopen failing — must not lose the backoff.
#[test]
fn draining_and_failed_reopens_keep_the_backoff() {
    let mut ledger = seeded_ledger();
    failures_until_eviction(&mut ledger);
    let (mut complete, mut healthy) = (true, true);
    for _ in 0..3 {
        settle_unhealthy_node(&mut ledger, &NODE, &mut complete, &mut healthy);
    }

    ledger.reset_channel_failures_after_open(&NODE);
    assert_eq!(failures_until_eviction(&mut ledger), 4);
}
