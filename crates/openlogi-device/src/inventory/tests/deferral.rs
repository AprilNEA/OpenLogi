//! Deferred probes: what a held register phase does to liveness, miss aging and warm cache entries.

use super::*;

#[test]
fn settling_mixed_probe_verdicts_preserves_liveness_and_neutral_deferrals() {
    use ProbeVerdict::{AliveButIncomplete, Deferred, Failed, Healthy};

    let mut ledger = super::ledger::NodeLedger::default();
    let known = inventory(&[1, 3]).pop().unwrap();
    let healthy = settle_probe(
        &mut ledger,
        &1,
        Healthy { complete: true },
        Some(known.clone()),
    );
    assert_eq!(healthy.inventory, Some(known.clone()));
    assert!(!healthy.evict_channel);

    // Only real incomplete probes age the three-tick snapshot grace. A live
    // receiver resets the ordinary two-failure streak, but its fourth failed
    // arrival replay still retires the channel. Deferrals change neither.
    for (tick, (verdict, replay, evict)) in [
        (Failed, true, false),
        (Deferred, true, false),
        (Deferred, true, false),
        (Deferred, true, false),
        (Deferred, true, false),
        (AliveButIncomplete, true, false),
        (Failed, true, false),
        (Deferred, true, false),
        (AliveButIncomplete, false, false),
        (Deferred, false, false),
        (AliveButIncomplete, false, false),
        (AliveButIncomplete, false, true),
    ]
    .into_iter()
    .enumerate()
    {
        let settled = settle_probe(&mut ledger, &1, verdict, None);
        assert_eq!(
            settled.inventory,
            replay.then(|| known.clone()),
            "tick {tick}"
        );
        assert_eq!(settled.evict_channel, evict, "tick {tick}");
    }
}

/// A deferred tick is evidence of nothing about the node's devices either:
/// the entries the node contributed are held out of miss aging, however many
/// deferrals run back to back, while the entries of a node that was actually
/// checked — and did not report them — age as before. Once the deferred node
/// is probed again, normal aging resumes from where it stood.
#[test]
fn deferred_ticks_hold_the_nodes_cache_entries_out_of_miss_aging() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let deferred_node = NodeId::from("deferred-receiver".to_string());
    let checked_node = NodeId::from("checked-receiver".to_string());
    let held = CacheKey::Bolt { unit_id: [1; 4] };
    let aged = CacheKey::Bolt { unit_id: [2; 4] };
    let answered = |outcomes| NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes,
    };
    // One pass's cache bookkeeping for a set of settled probes: the shape of
    // `enumerate_reporting_completeness`, without the channels.
    let pass = |e: &mut Enumerator, probes: Vec<(&NodeId, NodeProbe)>| {
        let mut frozen = HashSet::new();
        let mut outcomes = Vec::new();
        for (node, probe) in probes {
            settle_probe(&mut e.ledger, node, probe.verdict, probe.inventory.clone());
            e.hold_or_note_cache_keys(node, &probe, &mut frozen);
            outcomes.extend(probe.outcomes);
        }
        let seen = e.apply_outcomes(outcomes);
        e.evict_unseen(&seen, &frozen);
    };

    // Both nodes answer and contribute an entry each.
    pass(
        &mut e,
        vec![
            (
                &deferred_node,
                answered(vec![CacheOutcome::Fresh(held.clone(), cache_entry())]),
            ),
            (
                &checked_node,
                answered(vec![CacheOutcome::Fresh(aged.clone(), cache_entry())]),
            ),
        ],
    );

    // Then one pass past the grace in which the first node's probe is
    // deferred and the second answers without its device.
    for _ in 0..=CACHE_MISS_GRACE {
        pass(
            &mut e,
            vec![
                (&deferred_node, NodeProbe::deferred()),
                (&checked_node, answered(Vec::new())),
            ],
        );
    }
    assert!(
        e.cache.contains_key(&held),
        "a deferred node's entry must not age out"
    );
    assert!(
        !e.cache.contains_key(&aged),
        "a checked node's unreported entry ages as before"
    );
    assert!(
        !e.node_cache_keys[&checked_node].contains(&aged),
        "an evicted entry is no longer the node's to hold"
    );

    // The deferred node is probed again and does not report its device:
    // aging resumes.
    for _ in 0..=CACHE_MISS_GRACE {
        pass(&mut e, vec![(&deferred_node, answered(Vec::new()))]);
    }
    assert!(
        !e.cache.contains_key(&held),
        "normal aging resumes once the node is actually checked"
    );
}

/// A warm start loads persisted entries before any receiver has answered,
/// so a receiver deferred from its very first probe has no record of what is
/// its. Every unattributed entry is held for it until it is actually probed;
/// an entry another node has claimed is not.
#[test]
fn a_node_deferred_before_its_first_probe_holds_every_unattributed_entry() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let receiver = NodeId::from("warm-receiver".to_string());
    let checked_node = NodeId::from("checked-receiver".to_string());
    let persisted = CacheKey::Bolt { unit_id: [1; 4] };
    let claimed = CacheKey::Bolt { unit_id: [2; 4] };
    // The persisted entry was loaded, never attributed; another node
    // contributed — and now stops reporting — an entry of its own.
    e.cache.insert(persisted.clone(), cache_entry());
    let mut frozen = HashSet::new();
    let claim = NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes: vec![CacheOutcome::Fresh(claimed.clone(), cache_entry())],
    };
    e.hold_or_note_cache_keys(&checked_node, &claim, &mut frozen);
    e.apply_outcomes(claim.outcomes);

    for _ in 0..=CACHE_MISS_GRACE {
        let mut frozen = HashSet::new();
        let deferred = NodeProbe::deferred();
        settle_probe(&mut e.ledger, &receiver, deferred.verdict, None);
        e.hold_or_note_cache_keys(&receiver, &deferred, &mut frozen);
        let checked = NodeProbe {
            inventory: None,
            verdict: ProbeVerdict::Healthy { complete: true },
            outcomes: Vec::new(),
        };
        e.hold_or_note_cache_keys(&checked_node, &checked, &mut frozen);
        assert_eq!(
            frozen,
            HashSet::from([persisted.clone()]),
            "only the unattributed entry is held for the never-probed node"
        );
        let seen = e.apply_outcomes(deferred.outcomes);
        e.evict_unseen(&seen, &frozen);
    }
    assert!(
        e.cache.contains_key(&persisted),
        "a persisted entry must survive deferrals of the receiver that has yet to claim it"
    );
    assert!(
        !e.cache.contains_key(&claimed),
        "the checked node's unreported entry ages as before"
    );

    // The receiver's first real probe claims nothing: from then on its
    // deferrals hold nothing, and the persisted entry ages normally.
    let first = NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes: Vec::new(),
    };
    e.hold_or_note_cache_keys(&receiver, &first, &mut HashSet::new());
    for _ in 0..=CACHE_MISS_GRACE {
        let mut frozen = HashSet::new();
        e.hold_or_note_cache_keys(&receiver, &NodeProbe::deferred(), &mut frozen);
        assert!(
            frozen.is_empty(),
            "a probed node holds only what it claimed"
        );
        e.evict_unseen(&HashSet::new(), &frozen);
    }
    assert!(!e.cache.contains_key(&persisted));
}

#[test]
fn failed_first_probe_then_deferrals_preserve_warm_cache() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let receiver = NodeId::from("warm-failed-receiver".to_string());
    let persisted = CacheKey::Bolt { unit_id: [7; 4] };
    e.cache.insert(persisted.clone(), cache_entry());

    let pass = |e: &mut Enumerator, probe: NodeProbe| {
        let mut frozen = HashSet::new();
        e.hold_or_note_cache_keys(&receiver, &probe, &mut frozen);
        settle_probe(&mut e.ledger, &receiver, probe.verdict, probe.inventory);
        let seen = e.apply_outcomes(probe.outcomes);
        e.evict_unseen(&seen, &frozen);
    };

    // A timeout before identifying any slot is one real miss, but does not
    // establish which persisted entries belong to this receiver.
    pass(&mut e, NodeProbe::failed());
    assert_eq!(e.misses.get(&persisted), Some(&1));

    for _ in 0..CACHE_MISS_GRACE {
        pass(&mut e, NodeProbe::deferred());
    }
    assert!(
        e.cache.contains_key(&persisted),
        "deferrals after one failed probe must not delete the warm cache"
    );
    assert_eq!(e.misses.get(&persisted), Some(&1));
    assert!(!e.cache_dirty, "deferrals must not persist a deletion");

    // Real failures still age the entry, including the original miss.
    for _ in 1..CACHE_MISS_GRACE {
        pass(&mut e, NodeProbe::failed());
    }
    assert!(e.cache.contains_key(&persisted));
    pass(&mut e, NodeProbe::failed());
    assert!(!e.cache.contains_key(&persisted));
    assert!(e.cache_dirty);
}

#[test]
fn partial_first_probe_does_not_abandon_unread_slots_during_deferral() {
    let mut e = Enumerator::with_backend(ScriptedBackend::new(Vec::new()));
    let receiver = NodeId::from("warm-partial-receiver".to_string());
    let readable = CacheKey::Bolt {
        unit_id: [0, 0, 0, 1],
    };
    let unread = CacheKey::Bolt {
        unit_id: [0, 0, 0, 2],
    };
    e.cache.insert(readable.clone(), cache_entry());
    e.cache.insert(unread.clone(), cache_entry());

    let pass = |e: &mut Enumerator, probe: NodeProbe| {
        let mut frozen = HashSet::new();
        e.hold_or_note_cache_keys(&receiver, &probe, &mut frozen);
        settle_probe(&mut e.ledger, &receiver, probe.verdict, probe.inventory);
        let seen = e.apply_outcomes(probe.outcomes);
        e.evict_unseen(&seen, &frozen);
    };

    // The receiver reports two paired slots but only one identity is readable.
    // Its identifying outcome is not a complete cache-ownership inventory.
    let partial = assemble_bolt_probe(bolt_receiver_info(), Some(2), vec![bolt_slot(1)]);
    assert_eq!(partial.verdict, ProbeVerdict::Failed);
    pass(&mut e, partial);
    assert_eq!(e.misses.get(&unread), Some(&1));
    for _ in 0..CACHE_MISS_GRACE {
        pass(&mut e, NodeProbe::deferred());
    }
    assert!(e.cache.contains_key(&readable));
    assert!(
        e.cache.contains_key(&unread),
        "a partial first probe must not expose the unread slot to deferred miss aging"
    );
    assert_eq!(e.misses.get(&unread), Some(&1));
    assert!(!e.cache_dirty);

    // A later complete probe confirms only one pairing remains. Deferrals
    // can now protect only that slot, so the removed slot ages out normally.
    pass(
        &mut e,
        assemble_bolt_probe(bolt_receiver_info(), Some(1), vec![bolt_slot(1)]),
    );
    for _ in 1..CACHE_MISS_GRACE {
        pass(&mut e, NodeProbe::deferred());
    }
    assert!(e.cache.contains_key(&readable));
    assert!(!e.cache.contains_key(&unread));
    assert!(e.cache_dirty);
}
