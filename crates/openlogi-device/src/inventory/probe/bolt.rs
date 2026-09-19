//! Probing a Bolt receiver: its pairing registers name every occupied slot,
//! the arrival drain adds what is live, and the slots' feature tables are walked
//! concurrently once the register phase is released.

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures_concurrency::future::Join as _;
use hidpp::{
    channel::HidppChannel,
    receiver::bolt::{
        DeviceConnection as BoltDeviceConnection, Event as BoltEvent, Receiver as BoltReceiver,
    },
};
use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use tokio::time::timeout;
use tracing::{debug, warn};

use super::{MAX_BOLT_SLOTS, NodeProbe, PassContext, ProbeVerdict};
use crate::backend::NodeInfo;
use crate::host_lock::ReceiverRegisterPhase;
use crate::inventory::cache::{CacheKey, CacheOutcome, probe_or_reuse, seen};
use crate::inventory::events::EventSubscriptionHandle;
use crate::inventory::features::ProbedFeatures;
use crate::inventory::mappings::{map_kind, resolve_device_kind};

/// Probe a Bolt receiver under its register phase, which is dropped once the
/// register reads are done: the slot walks that follow address each device
/// by index under this process's own software id, and another process may
/// have the receiver's registers meanwhile.
pub(super) async fn probe_bolt_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    bolt: BoltReceiver,
    registers: ReceiverRegisterPhase,
    pass: PassContext<'_>,
) -> NodeProbe {
    let unique_id = bolt.get_unique_id().await.ok();
    let pairing_count = bolt.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    let connections = drain_device_arrival(
        &bolt,
        pass.subscriptions,
        pass.timeouts.arrival_drain_timeout,
    )
    .await;
    debug!(events = connections.len(), "drained device-arrival events");
    let by_slot: HashMap<u8, BoltDeviceConnection> =
        connections.into_iter().map(|c| (c.index, c)).collect();

    // Phase 1 — read each occupied slot's identity from the receiver,
    // sequentially. These reads all address the receiver (index 0xff), and the
    // channel correlates responses by register, not by the slot in the request
    // payload, so overlapping them could hand one slot's response to another
    // (wrong unit id / online / kind). They are cheap register reads, so
    // serializing them costs little.
    let mut identities = Vec::new();
    for slot in 1u8..=MAX_BOLT_SLOTS {
        if let Some(identity) =
            read_bolt_slot_identity(&bolt, &channel, by_slot.get(&slot), slot).await
        {
            identities.push(identity);
        }
    }
    drop(registers);

    // Phase 2 — walk each occupied slot's feature table concurrently. Every walk
    // addresses its own device index, so responses route by index (no
    // cross-talk), and this per-device walk is the slow part a laggy device
    // would otherwise serialize the rest of the receiver behind. Each is bounded
    // independently by `bolt_slot_probe_timeout`; the ordered identity list keeps the
    // device list stable across ticks without an explicit sort.
    let slot_results = identities
        .iter()
        .map(|identity| walk_bolt_slot(&channel, identity, pass))
        .collect::<Vec<_>>()
        .join()
        .await;

    let receiver = ReceiverInfo {
        name: "Logi Bolt Receiver".to_string(),
        vendor_id: info.vendor_id,
        product_id: info.product_id,
        unique_id,
    };
    assemble_bolt_probe(receiver, pairing_count, slot_results)
}

/// Fold a Bolt receiver's per-slot results into a [`NodeProbe`].
///
/// `slot_results` holds one entry per *occupied* slot in slot order — empty or
/// unreadable slots are dropped in phase 1 ([`read_bolt_slot_identity`]) and
/// never reach here. The verdict is `Healthy` only when the pairing-count
/// register answered AND every counted slot was readable: `None` (the
/// receiver didn't answer, e.g. a parked channel) or a shortfall is "couldn't
/// fully check", so the ledger replays the last good snapshot instead of
/// presenting the partial walk as the new truth (#218). A slot whose feature
/// walk merely timed out still counts here — it falls back to cached/identity
/// data in [`walk_bolt_slot`].
pub(in crate::inventory) fn assemble_bolt_probe(
    receiver: ReceiverInfo,
    pairing_count: Option<u8>,
    slot_results: Vec<(PairedDevice, CacheOutcome)>,
) -> NodeProbe {
    let (paired, outcomes): (Vec<_>, Vec<_>) = slot_results.into_iter().unzip();

    if let Some(count) = pairing_count
        && paired.len() != usize::from(count)
    {
        warn!(
            expected = count,
            found = paired.len(),
            "paired-device count mismatch — some slots may be unreadable"
        );
    }
    let answered_in_full = pairing_count.is_some_and(|count| paired.len() == usize::from(count));

    NodeProbe {
        inventory: Some(DeviceInventory { receiver, paired }),
        verdict: ProbeVerdict::healthy_when(answered_in_full),
        outcomes,
    }
}

/// Identity read from the receiver's registers for one occupied Bolt slot
/// (phase 1). Both reads address the receiver at index `0xff`, and the channel
/// correlates responses by register — not by the slot encoded in the request
/// payload — so they must be issued sequentially, never overlapped across slots.
struct BoltSlotIdentity {
    slot: u8,
    codename: Option<String>,
    /// Cache key from the pairing register's unit id. `None` = all-zero id
    /// (unidentifiable): don't cache; always probe when online.
    id: Option<CacheKey>,
    online: bool,
    register_kind: DeviceKind,
    wpid: Option<u16>,
}

/// Read one Bolt slot's identity from the receiver's pairing + codename
/// registers. Returns `None` when the slot is empty or its pairing register
/// didn't read this tick. Must be called sequentially across slots — see
/// [`probe_bolt_receiver`].
async fn read_bolt_slot_identity(
    bolt: &BoltReceiver,
    channel: &Arc<HidppChannel>,
    event: Option<&BoltDeviceConnection>,
    slot: u8,
) -> Option<BoltSlotIdentity> {
    let pairing = match bolt.get_device_pairing_information(slot).await {
        Ok(p) => p,
        Err(e) => {
            debug!(slot, error = ?e, "slot empty or unreadable");
            return None;
        }
    };
    let codename = read_codename(channel, slot).await;
    // Prefer event data when present — it's a live response. Fall back to the
    // pairing register for sleeping devices that didn't reply.
    let online = event.map_or(pairing.online, |c| c.online);
    let bolt_kind = event.map_or(pairing.kind, |c| c.kind);
    let wpid = event.map(|c| c.wpid);
    debug!(
        slot,
        online,
        ?wpid,
        ?bolt_kind,
        has_event = event.is_some(),
        codename = ?codename,
        "paired slot"
    );

    // The pairing register gives the device's unit id cheaply every tick — its
    // stable cache identity. An all-zero id is treated as unidentifiable (don't
    // cache; always probe when online).
    let id = (pairing.unit_id != [0u8; 4]).then_some(CacheKey::Bolt {
        unit_id: pairing.unit_id,
    });
    Some(BoltSlotIdentity {
        slot,
        codename,
        id,
        online,
        register_kind: map_kind(bolt_kind),
        wpid,
    })
}

/// Walk one identified Bolt slot's HID++ feature table (phase 2). Addresses the
/// device at its own index, so this is safe to run concurrently across slots.
/// Always yields the device — a timed-out or failed walk falls back to the
/// slot's cached / identity-only data — plus its cache contribution this tick.
async fn walk_bolt_slot(
    channel: &Arc<HidppChannel>,
    identity: &BoltSlotIdentity,
    pass: PassContext<'_>,
) -> (PairedDevice, CacheOutcome) {
    let &BoltSlotIdentity {
        slot,
        online,
        register_kind,
        wpid,
        ..
    } = identity;
    let id = identity.id.clone();
    let cached = id.as_ref().and_then(|i| pass.cache.get(i));

    // Cap the feature walk per slot so one device that stops answering can't
    // burn the whole receiver's budget and time out `probe_one` — which would
    // drop *every* device on the receiver. A timed-out slot falls back to its
    // cached probe (its pairing-register identity read fine in phase 1),
    // mirroring the Unifying path (#218).
    let slot_budget = pass.timeouts.bolt_slot_probe_timeout;
    let probe_result = timeout(
        slot_budget,
        probe_or_reuse(
            channel,
            slot,
            id.clone(),
            cached,
            online,
            pass.now,
            pass.subscriptions,
        ),
    )
    .await;
    let (probe, outcome) = if let Ok(r) = probe_result {
        r
    } else {
        debug!(slot, budget = ?slot_budget,
            "Bolt slot probe timed out; using cached data if available");
        let probe = cached.map_or_else(ProbedFeatures::default, |c| c.probe.clone());
        (probe, seen(id))
    };
    if matches!(outcome, CacheOutcome::Fresh(..))
        && let Some(probed) = probe.kind
        && probed != DeviceKind::Unknown
        && register_kind != DeviceKind::Unknown
        && probed != register_kind
    {
        debug!(
            slot,
            ?register_kind,
            ?probed,
            "device-kind sources disagree — trusting 0x0005"
        );
    }

    let device = PairedDevice {
        slot,
        codename: identity.codename.clone(),
        wpid,
        // Prefer the device's own `0x0005` type; the register kind is the
        // offline fallback.
        kind: resolve_device_kind(probe.kind, register_kind),
        online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    };
    (device, outcome)
}

async fn drain_device_arrival(
    bolt: &BoltReceiver,
    subscriptions: Option<&EventSubscriptionHandle>,
    idle_timeout: Duration,
) -> Vec<BoltDeviceConnection> {
    let rx = bolt.listen();
    // Triggering a snapshot fabricates the same connection messages as a real
    // lifecycle event. Suppress raw reconciliation requests only during this
    // drain, whose typed receiver consumes every such message into the current
    // snapshot. Drop the guard before slot probes so a later transition cannot
    // be lost behind unrelated feature reads.
    let _receiver_snapshot = subscriptions.map(EventSubscriptionHandle::begin_receiver_snapshot);
    match bolt.get_notification_state().await {
        Ok(mut state) if !state.wireless_notifications => {
            state.wireless_notifications = true;
            if let Err(error) = bolt.set_notification_state(state).await {
                debug!(?error, "enable Bolt wireless notifications failed");
            }
        }
        Ok(_) => {}
        Err(error) => debug!(?error, "read Bolt notification state failed"),
    }
    if let Err(e) = bolt.trigger_device_arrival().await {
        debug!(error = ?e, "trigger_device_arrival failed; receiver may report no devices");
        return Vec::new();
    }

    let mut out = Vec::new();
    loop {
        match timeout(idle_timeout, rx.recv()).await {
            Ok(Ok(BoltEvent::DeviceConnection(c))) => out.push(c),
            Ok(Ok(_)) => {} // BoltEvent is non_exhaustive; ignore future variants
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// Reads a paired device's codename, working around a slicing bug in
/// `hidpp 0.2`'s `BoltReceiver::get_device_codename` that truncates names
/// longer than 8 characters (it treats `response[2]` as an end-index when it
/// is actually the byte length — see Solaar's `device_codename` for the
/// correct slice). 16-byte long-register response is `[sub, chunk, len,
/// data..13]`; we cap at 13 to stay in-bounds. Long names (>13 chars) would
/// need multi-chunk reads with chunk param > 0x01; not needed for v0.0.x.
async fn read_codename(channel: &HidppChannel, slot: u8) -> Option<String> {
    // 0xFF = receiver device index, 0xB5 = ReceiverInfo register,
    // 0x60+slot = DeviceCodename sub-register, 0x01 = first chunk.
    let response = channel
        .read_long_sub_register(0xFF, 0xB5, 0x60 + slot, [0x01, 0x00])
        .await
        .ok()?;
    let len = usize::from(response[2]).min(13);
    core::str::from_utf8(&response[3..3 + len])
        .ok()
        .map(str::to_string)
}
