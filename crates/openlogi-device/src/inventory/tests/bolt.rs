//! The Bolt receiver probe: slot completeness, and the register-phase wait against the I/O budget.

use super::*;

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
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Healthy { complete: true },
        "a count matching the readable slots is authoritative and complete"
    );
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
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Failed,
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
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Failed,
        "no count register means we couldn't fully check"
    );
}

/// The Logi Bolt receiver's product id.
const BOLT_RECEIVER_PID: u16 = 0xc548;

/// The unit id [`bolt_receiver_with_a_silent_slot`] reports for slot 1.
const SILENT_SLOT_UNIT_ID: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];

/// A Bolt receiver with one paired mouse in slot 1. The receiver answers
/// every register read at once — no arrival events, so the drain runs to its
/// deadline and the slot is read from the pairing register — while the mouse
/// itself, addressed at its slot index, never answers: a slot whose feature
/// walk runs to its own budget and falls back to the cache.
fn bolt_receiver_with_a_silent_slot(request: &[u8]) -> Option<Vec<u8>> {
    let [_, device, sub_id, address, sub_register, ..] = *request else {
        return None;
    };
    if device != 0xff {
        return None;
    }
    let short = |data: [u8; 3]| Some(vec![0x10, 0xff, sub_id, address, data[0], data[1], data[2]]);
    let long = |data: &[u8]| {
        let mut report = vec![0x11, 0xff, 0x83, address];
        report.extend_from_slice(data);
        report.resize(20, 0);
        Some(report)
    };
    match (sub_id, address) {
        // Notifications (wireless notifications already on) and Connections
        // (one pairing) happen to read the same.
        (0x81, 0x00 | 0x02) => short([0x00, 0x01, 0x00]),
        // Register writes (the arrival trigger) are acknowledged.
        (0x80, _) => short([0x00, 0x00, 0x00]),
        // Unique id: sixteen ASCII bytes.
        (0x83, 0xfb) => long(b"0000000012345678"),
        (0x83, 0xb5) => match sub_register {
            // Slot 1's pairing information: a mouse, online, wpid c09d.
            0x51 => long(&[0x51, 0x02, 0x9d, 0xc0, 0xde, 0xad, 0xbe, 0xef]),
            // Slot 1's codename.
            0x61 => long(&[
                0x61, 0x01, 0x08, b'M', b'X', b' ', b'P', b'r', b'o', b'b', b'e',
            ]),
            // Every other slot is empty: an error reply, no sub-register byte.
            _ => Some(vec![0x10, 0xff, 0x8f, 0x83, 0xb5, 0x08, 0x00]),
        },
        _ => None,
    }
}

/// A Bolt receiver node whose register phase no other test shares.
fn bolt_receiver_node(tag: &str) -> NodeInfo {
    let mut info = scripted_node_info(&format!("{tag}-{}", std::process::id()));
    info.product_id = BOLT_RECEIVER_PID;
    info
}

/// Timeouts shrunk to test scale, in the production proportions: the slot
/// probe and the drain fit the receiver budget with room, and the register
/// lock wait is long enough for a holder to release inside it.
fn quick_timeouts() -> ProbeTimeouts {
    ProbeTimeouts {
        register_lock: Duration::from_secs(2),
        receiver: Duration::from_millis(900),
        direct: Duration::from_millis(900),
        arrival_drain: Duration::from_millis(100),
        bolt_slot_probe: Duration::from_millis(400),
        unifying_slot_probe: Duration::from_millis(400),
        unifying_cached_slot_probe: Duration::from_millis(100),
    }
}

/// A stale cache entry for the silent slot, so its walk runs and has
/// something to fall back to when it times out.
fn stale_silent_slot_cache() -> (CacheKey, HashMap<CacheKey, Cached>) {
    let key = CacheKey::Bolt {
        unit_id: SILENT_SLOT_UNIT_ID,
    };
    let mut entry = cache_entry();
    entry.probe = probed(Some(model(SILENT_SLOT_UNIT_ID, Some("SN-1"))), false);
    entry.probed_at = Instant::now()
        .checked_sub(REFRESH_INTERVAL)
        .expect("the process has been up longer than the refresh interval's worth of ticks");
    let cache = HashMap::from([(key.clone(), entry)]);
    (key, cache)
}

/// The register-phase wait sits outside the receiver's I/O budget: a probe
/// that waited for another process to release the phase — inside its wait
/// budget — still gets the whole I/O budget the receiver's worst case was
/// sized for, so a slow slot reaches its normal timeout-and-cache fallback
/// and the receiver settles healthy. Composed with the wait plus the I/O
/// exceeding the budget on purpose: under one deadline around both, this
/// probe failed — and two such failures retire a working receiver's channel.
#[tokio::test]
async fn a_receiver_probe_that_waited_for_its_register_phase_keeps_its_whole_io_budget() {
    let info = bolt_receiver_node("register-phase-wait");
    let timeouts = quick_timeouts();
    let lock_held_for = Duration::from_millis(600);
    // Another process (here: this test) holds the receiver's register phase,
    // releasing it inside the wait but late enough that wait + I/O outruns
    // the receiver budget.
    let held = host_lock::try_lock(&host_lock::node_lock_name(&info.id))
        .unwrap()
        .expect("the test takes the phase first");
    let release = tokio::spawn(async move {
        tokio::time::sleep(lock_held_for).await;
        drop(held);
    });
    let (raw, _handle) = ScriptedRawHidChannel::with_responder(bolt_receiver_with_a_silent_slot);
    let channel = scripted_channel(raw.presenting_as(BOLT_RECEIVER_PID)).await;
    let (key, cache) = stale_silent_slot_cache();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &timeouts,
    };

    let started = Instant::now();
    let probe = probe_one(info, channel, pass).await;
    release.await.unwrap();

    let io_floor = timeouts.arrival_drain + timeouts.bolt_slot_probe;
    assert!(
        lock_held_for + io_floor > timeouts.receiver,
        "the test must compose a wait and an I/O floor that together outrun the budget"
    );
    assert!(
        started.elapsed() >= lock_held_for + io_floor,
        "the probe waited for the phase and the slot ran to its own budget: {:?}",
        started.elapsed()
    );
    assert_eq!(
        probe.verdict,
        ProbeVerdict::Healthy { complete: true },
        "the wait must not have eaten into the I/O budget"
    );
    let inventory = probe.inventory.expect("the receiver answered");
    assert_eq!(
        inventory.receiver.unique_id.as_deref(),
        Some("0000000012345678")
    );
    assert_eq!(inventory.paired.len(), 1);
    let device = &inventory.paired[0];
    assert_eq!(device.slot, 1);
    assert_eq!(device.codename.as_deref(), Some("MX Probe"));
    assert!(device.online);
    assert_eq!(
        device.model_info.as_ref().map(|m| m.unit_id),
        Some(SILENT_SLOT_UNIT_ID),
        "the timed-out slot fell back to its cached probe"
    );
    assert!(
        matches!(probe.outcomes.as_slice(), [CacheOutcome::Seen(seen)] if *seen == key),
        "a timed-out slot keeps its cache entry alive without refreshing it"
    );
}

/// A receiver whose register phase stays held past the wait is deferred
/// without a byte of I/O: nothing was checked, so nothing is reported as
/// failed.
#[tokio::test]
async fn a_receiver_probe_defers_when_the_register_phase_is_held_past_the_wait() {
    let info = bolt_receiver_node("register-phase-held");
    let timeouts = ProbeTimeouts {
        register_lock: Duration::from_millis(100),
        ..quick_timeouts()
    };
    let _held = host_lock::try_lock(&host_lock::node_lock_name(&info.id))
        .unwrap()
        .expect("the test takes the phase first");
    let (raw, handle) = ScriptedRawHidChannel::with_responder(bolt_receiver_with_a_silent_slot);
    let channel = scripted_channel(raw.presenting_as(BOLT_RECEIVER_PID)).await;
    let cache = HashMap::new();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &timeouts,
    };

    let probe = probe_one(info, channel, pass).await;

    assert_eq!(probe.verdict, ProbeVerdict::Deferred);
    assert!(probe.inventory.is_none());
    assert!(probe.outcomes.is_empty());
    assert!(
        handle.written_reports().is_empty(),
        "a deferred probe must not touch the receiver"
    );
}

/// The I/O budget still bounds the probe on its own: a receiver whose slot
/// walk alone outruns it is a failed probe, for the ledger to replay through.
#[tokio::test]
async fn a_receiver_probe_whose_io_outruns_the_budget_is_failed() {
    let info = bolt_receiver_node("io-outruns-budget");
    let timeouts = ProbeTimeouts {
        receiver: Duration::from_millis(150),
        ..quick_timeouts()
    };
    let (raw, _handle) = ScriptedRawHidChannel::with_responder(bolt_receiver_with_a_silent_slot);
    let channel = scripted_channel(raw.presenting_as(BOLT_RECEIVER_PID)).await;
    let (_, cache) = stale_silent_slot_cache();
    let pass = PassContext {
        cache: &cache,
        now: Instant::now(),
        subscriptions: None,
        timeouts: &timeouts,
    };

    let probe = probe_one(info, channel, pass).await;

    assert_eq!(probe.verdict, ProbeVerdict::Failed);
    assert!(probe.inventory.is_none());
}
