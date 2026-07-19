use std::collections::HashSet;

use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};

use super::cache::{CACHE_MISS_GRACE, CacheKey, Cached, REFRESH_TICKS, is_stale};
use super::probe::parse_codename_unifying;
use super::{Enumerator, ONESHOT_ATTEMPTS, one_shot_should_stop};
use crate::inventory::features::ProbedFeatures;

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

fn inventory(slots: &[u8]) -> Vec<DeviceInventory> {
    vec![DeviceInventory {
        receiver: ReceiverInfo {
            name: "Unifying Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc52b,
            unique_id: Some("receiver-1".to_string()),
        },
        paired: slots
            .iter()
            .copied()
            .map(|slot| PairedDevice {
                slot,
                codename: Some(format!("device-{slot}")),
                wpid: Some(0xb000 + u16::from(slot)),
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: None,
                capabilities: None,
            })
            .collect(),
    }]
}

#[test]
fn one_shot_retry_stops_when_first_attempt_is_complete() {
    let current = inventory(&[1, 2]);

    assert!(
        one_shot_should_stop(None, &current, true, true, 1),
        "complete inventories keep the one-pass happy path"
    );
}

#[test]
fn one_shot_retry_waits_for_healthy_incomplete_inventory_to_stabilize() {
    let partial = inventory(&[1]);
    let full = inventory(&[1, 2]);

    assert!(
        !one_shot_should_stop(None, &partial, false, true, 1),
        "the first incomplete pass has no previous inventory to compare"
    );
    assert!(
        !one_shot_should_stop(Some(partial.as_slice()), &full, false, true, 2),
        "a changed inventory should get another retry window"
    );
    assert!(
        one_shot_should_stop(Some(full.as_slice()), &full, false, true, 3),
        "once the returned inventory stabilizes, retrying stops"
    );
}

#[test]
fn one_shot_retry_stops_on_unchanged_incomplete_inventory() {
    let partial = inventory(&[1]);

    assert!(
        one_shot_should_stop(Some(partial.as_slice()), &partial, false, true, 2),
        "stable partial inventories should not burn every retry attempt"
    );
}

#[test]
fn one_shot_retry_keeps_unchanged_inventory_after_unhealthy_probe() {
    let partial = inventory(&[1]);

    assert!(
        !one_shot_should_stop(Some(partial.as_slice()), &partial, false, false, 2),
        "unchanged replay after a failed probe must keep retrying before the cap"
    );
}

#[test]
fn one_shot_retry_stops_at_attempt_cap_when_inventory_keeps_changing() {
    let previous = inventory(&[1]);
    let current = inventory(&[1, 2]);

    assert!(
        one_shot_should_stop(
            Some(previous.as_slice()),
            &current,
            false,
            false,
            ONESHOT_ATTEMPTS
        ),
        "the retry loop must remain bounded even if the inventory changes every time"
    );
}

#[test]
fn codename_reads_len_prefixed_name() {
    // wire-verified MX Master 2S reply: `40 0c "MX Master 2S"` then padding.
    let mut buf = vec![0x40, 0x0c];
    buf.extend_from_slice(b"MX Master 2S");
    buf.extend_from_slice(&[0u8; 2]); // trailing bytes of the 16-byte register
    assert_eq!(
        parse_codename_unifying(&buf).as_deref(),
        Some("MX Master 2S")
    );
}

#[test]
fn codename_clamps_overlong_len() {
    // a bogus length byte must not over-read past the buffer.
    let buf = [0x40, 0xff, b'h', b'i'];
    assert_eq!(parse_codename_unifying(&buf).as_deref(), Some("hi"));
}

#[test]
fn codename_rejects_short_response() {
    assert_eq!(parse_codename_unifying(&[0x40]), None);
}
