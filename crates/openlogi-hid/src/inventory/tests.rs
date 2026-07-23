use std::{collections::HashSet, error::Error, io, sync::Arc};

use hidpp::channel::{HidppChannel, RawHidChannel};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
};
use tokio::sync::{Mutex, mpsc};

use super::cache::{CACHE_MISS_GRACE, CacheKey, CacheOutcome, Cached, REFRESH_TICKS, is_stale};
use super::probe::{
    NodeProbe, assemble_bolt_probe, assemble_unifying_device, parse_codename_unifying,
    probe_unifying_features,
};
use super::{Enumerator, ONESHOT_ATTEMPTS, one_shot_should_stop, retained_nodes};
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
fn live_cached_channel_survives_a_transient_enumeration_gap() {
    let enumerated = HashSet::from([1]);
    let cached_channels = [(1, true), (2, true), (3, false)];

    let retained = retained_nodes(&enumerated, cached_channels);

    assert_eq!(retained, HashSet::from([1, 2]));
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
fn unifying_cached_features_do_not_override_current_liveness() {
    let probe = ProbedFeatures {
        capabilities: Some(Capabilities::default()),
        ..ProbedFeatures::default()
    };

    let device = assemble_unifying_device(
        1,
        Some("cached mouse".to_string()),
        0x4082,
        DeviceKind::Mouse,
        probe,
        false,
    );

    assert!(
        !device.online,
        "the current liveness probe is authoritative"
    );
    assert!(
        device.capabilities.is_some(),
        "cached immutable features remain available while offline"
    );
}

struct BatteryErrorPingChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
}

impl BatteryErrorPingChannel {
    fn new() -> Self {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        Self {
            incoming_tx,
            incoming_rx: Mutex::new(incoming_rx),
        }
    }
}

#[hidpp::async_trait]
impl RawHidChannel for BatteryErrorPingChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xc52b
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let mut response = src.to_vec();
        if src.get(2).copied() != Some(0) {
            if response.len() < 7 {
                return Err(Box::new(io::Error::other("short mock HID++ report")));
            }
            response[2] = 0xff;
            response[3] = src[2];
            response[4] = src[3];
            response[5] = 0x08;
            response[6] = 0;
        }
        self.incoming_tx.send(response).map_err(|_| {
            Box::new(io::Error::other("mock HID++ response receiver closed"))
                as Box<dyn Error + Send + Sync>
        })?;
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return Err(Box::new(io::Error::other(
                "mock HID++ response sender closed",
            )));
        };
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        unreachable!("mock declares HID++ support")
    }
}

#[tokio::test]
async fn unifying_battery_failure_uses_root_ping_before_marking_offline() {
    let channel = Arc::new(
        HidppChannel::from_raw_channel(BatteryErrorPingChannel::new())
            .await
            .unwrap_or_else(|e| panic!("mock HID++ channel should open: {e}")),
    );
    let id = CacheKey::UnifyingSlot {
        receiver_uid: "receiver".to_string(),
        slot: 1,
    };
    let cached = Cached {
        probe: ProbedFeatures {
            capabilities: Some(Capabilities::default()),
            ..ProbedFeatures::default()
        },
        battery_index: Some(4),
        probed_tick: 10,
    };

    let (_, _, online) = probe_unifying_features(&channel, 1, &id, Some(&cached), 11).await;

    assert!(
        online,
        "a failed battery refresh is inconclusive when the root ping still answers"
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

fn bolt_receiver_info() -> ReceiverInfo {
    ReceiverInfo {
        name: "Logi Bolt Receiver".to_string(),
        vendor_id: 0x046d,
        product_id: 0xc548,
        unique_id: Some("bolt-1".to_string()),
    }
}

/// A readable slot's probe result. `Seen` models the fallback a feature-walk
/// timeout produces (#251): the device still surfaces from its pairing-register
/// identity, so a timed-out slot counts as readable here.
fn bolt_slot(slot: u8) -> (PairedDevice, CacheOutcome) {
    (
        PairedDevice {
            slot,
            codename: Some(format!("device-{slot}")),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        },
        CacheOutcome::Seen(CacheKey::Bolt {
            unit_id: [0, 0, 0, slot],
        }),
    )
}

fn paired_slots(probe: &NodeProbe) -> Vec<u8> {
    let Some(inventory) = probe.inventory.as_ref() else {
        panic!("expected an inventory");
    };
    inventory.paired.iter().map(|d| d.slot).collect()
}

#[test]
fn bolt_probe_is_complete_when_count_matches_readable_slots() {
    // Two paired slots, both readable, and the pairing-count register agrees.
    // Empty slots are dropped in phase 1, so only occupied slots reach here;
    // `join` yields them in slot order, so the devices must come out ordered
    // without an explicit sort.
    let probe = assemble_bolt_probe(
        bolt_receiver_info(),
        Some(2),
        vec![bolt_slot(1), bolt_slot(2)],
    );
    assert!(probe.complete, "count matches the readable slots");
    assert!(probe.healthy, "a complete Bolt walk is authoritative");
    assert_eq!(paired_slots(&probe), vec![1, 2], "slots surface in order");
    assert_eq!(
        probe.outcomes.len(),
        2,
        "one cache outcome per readable slot"
    );
}

#[test]
fn bolt_probe_is_incomplete_when_a_counted_slot_is_unreadable() {
    // The receiver reports two paired devices but only one slot's pairing
    // register read this tick. Presenting that partial walk as the new truth is
    // the #218 regression: it must stay incomplete so the ledger replays the
    // last good snapshot instead of dropping the missing device.
    let probe = assemble_bolt_probe(bolt_receiver_info(), Some(2), vec![bolt_slot(1)]);
    assert_eq!(
        paired_slots(&probe),
        vec![1],
        "only the readable slot surfaces"
    );
    assert!(!probe.complete, "a count shortfall is not complete");
    assert!(
        !probe.healthy,
        "an incomplete Bolt walk is not authoritative"
    );
}

#[test]
fn bolt_probe_is_incomplete_when_the_count_register_is_unanswered() {
    // A parked/unresponsive receiver channel returns no pairing count. Even with
    // slots surfaced from arrival events, the walk can't be trusted as the whole
    // truth, so it stays incomplete and the ledger keeps the prior snapshot.
    let probe = assemble_bolt_probe(bolt_receiver_info(), None, vec![bolt_slot(1), bolt_slot(2)]);
    assert_eq!(paired_slots(&probe), vec![1, 2]);
    assert!(
        !probe.complete,
        "no count register means we couldn't fully check"
    );
    assert!(!probe.healthy);
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
