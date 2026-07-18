use std::{collections::HashMap, sync::Arc};

use futures_concurrency::future::Join as _;
use hidpp::{
    channel::HidppChannel,
    receiver::{
        self, Receiver,
        bolt::{
            DeviceConnection as BoltDeviceConnection, Event as BoltEvent, Receiver as BoltReceiver,
        },
        unifying::{
            DeviceConnection as UnifyingDeviceConnection, Event as UnifyingEvent,
            Receiver as UnifyingReceiver,
        },
    },
};
use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::mappings::{map_kind, map_unifying_kind, resolve_device_kind};
use crate::route::DIRECT_DEVICE_INDEX;

use super::cache::{CacheKey, CacheOutcome, Cached, probe_or_reuse, seen};
use super::features::ProbedFeatures;
use super::{ARRIVAL_DRAIN, BOLT_SLOT_PROBE, MAX_BOLT_SLOTS, UNIFYING_SLOT_PROBE};

/// One probed node's contribution this tick: its inventory (if any), whether
/// the node actually answered — the ledger replays the last snapshot when it
/// didn't (see [`crate::node_ledger::NodeLedger::settle`]) — and each device's
/// cache contribution, for the caller to apply and to drive eviction.
pub(super) struct NodeProbe {
    pub(super) inventory: Option<DeviceInventory>,
    pub(super) healthy: bool,
    pub(super) outcomes: Vec<CacheOutcome>,
}

impl NodeProbe {
    /// A probe that got no answer at all (budget timeout).
    pub(super) fn failed() -> Self {
        Self {
            inventory: None,
            healthy: false,
            outcomes: Vec::new(),
        }
    }
}

/// Probe one open HID++ node (channel reused across ticks by the caller).
pub(super) async fn probe_one(
    info: async_hid::DeviceInfo,
    channel: Arc<HidppChannel>,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    match receiver::detect(Arc::clone(&channel)) {
        Some(Receiver::Bolt(bolt)) => probe_bolt_receiver(channel, info, bolt, cache, tick).await,
        Some(Receiver::Unifying(unifying)) => {
            probe_unifying_receiver(channel, info, unifying, cache, tick).await
        }
        None | Some(_) => {
            // No recognised receiver — this might be a directly-paired device
            // (Bluetooth-direct, USB-C cable). HID++ at device-index 0xff
            // addresses the device's own features. Probe in case it answers.
            // P2.4 — verified path; no Bolt-pairing slot indirection needed.
            probe_direct(channel, &info, cache, tick).await
        }
    }
}

async fn probe_bolt_receiver(
    channel: Arc<HidppChannel>,
    info: async_hid::DeviceInfo,
    bolt: BoltReceiver,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    let unique_id = bolt.get_unique_id().await.ok();
    let pairing_count = bolt.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    let connections = drain_device_arrival(&bolt).await;
    debug!(events = connections.len(), "drained device-arrival events");
    let by_slot: HashMap<u8, BoltDeviceConnection> =
        connections.into_iter().map(|c| (c.index, c)).collect();

    let mut paired = Vec::new();
    let mut outcomes = Vec::new();
    for slot in 1u8..=MAX_BOLT_SLOTS {
        if let Some((device, outcome)) =
            probe_bolt_slot(&channel, &bolt, by_slot.get(&slot), slot, cache, tick).await
        {
            paired.push(device);
            outcomes.push(outcome);
        }
    }

    if let Some(count) = pairing_count
        && paired.len() != usize::from(count)
    {
        warn!(
            expected = count,
            found = paired.len(),
            "paired-device count mismatch — some slots may be unreadable"
        );
    }
    // Authoritative only when the pairing-count register answered AND every
    // counted slot was readable. `None` (the receiver didn't answer — e.g. a
    // parked channel) or a shortfall is "couldn't fully check": the ledger
    // then replays the last good snapshot instead of presenting the partial
    // walk as the new truth (#218).
    let complete = pairing_count.is_some_and(|count| paired.len() == usize::from(count));

    NodeProbe {
        inventory: Some(DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_string(),
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                unique_id,
            },
            paired,
        }),
        healthy: complete,
        outcomes,
    }
}

async fn probe_unifying_receiver(
    channel: Arc<HidppChannel>,
    info: async_hid::DeviceInfo,
    unifying: UnifyingReceiver,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    let unique_id = unifying.get_unique_id().await.ok();
    let pairing_count = unifying.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    // Trigger device-arrival events and collect one event per online device.
    // Each event carries the slot index, kind, wpid, and online flag — enough
    // to build a PairedDevice entry for every currently-connected device.
    //
    // Note: the Unifying `0xB5/0x5N` pairing-info register uses a different
    // sub-register base than Bolt, so we don't yet poll offline paired slots.
    // Online devices are covered by the arrival drain; offline device support
    // requires resolving the correct sub-register format.
    //
    // The drain is therefore the *only* device source on this path, so a
    // failed arrival trigger is "couldn't check", not "no devices online":
    // settle it as a failed probe and let the ledger replay the last snapshot.
    let Some(connections) = drain_device_arrival_unifying(&unifying).await else {
        return NodeProbe::failed();
    };
    debug!(events = connections.len(), "drained device-arrival events");

    // Probe all online slots concurrently so a slow HID++ 2.0 feature walk on
    // one device doesn't push the next slot past the PROBE_BUDGET deadline.
    // Pass the receiver UID so each slot's cache key is scoped to this specific
    // receiver — two Unifying receivers sharing a slot number must not share a
    // cache entry (different devices, different capabilities).
    let receiver_uid_fallback;
    let receiver_uid = if let Some(uid) = unique_id.as_deref() {
        uid
    } else {
        // UID fetch failed — use the product ID as a weaker discriminant so
        // two receivers with the same PID still collide, but a receiver and a
        // direct device never share a cache entry.
        tracing::warn!("Unifying receiver UID unavailable; cache isolation may be degraded");
        receiver_uid_fallback = format!("pid:{:04x}", info.product_id);
        &receiver_uid_fallback
    };
    let slot_results = connections
        .iter()
        .map(|conn| probe_unifying_slot(&channel, conn, receiver_uid, cache, tick))
        .collect::<Vec<_>>()
        .join()
        .await;

    let (paired, outcomes): (Vec<_>, Vec<_>) = slot_results.into_iter().flatten().unzip();

    if let Some(count) = pairing_count
        && paired.len() != usize::from(count)
    {
        debug!(
            expected = count,
            found = paired.len(),
            "online devices differ from pairing count; offline devices not yet surfaced for Unifying"
        );
    }
    // Unlike Bolt, a count/list shortfall is *expected* here (offline paired
    // devices aren't enumerable yet), so completeness can't ride on it. The
    // health signal is the pairing-count register answering at all: that
    // proves the receiver round-trip worked this cycle, while `None` (e.g. a
    // parked channel) is "couldn't fully check" — the ledger then replays the
    // last good snapshot instead of presenting a possibly-empty list (#218).
    let healthy = pairing_count.is_some();

    NodeProbe {
        inventory: Some(DeviceInventory {
            receiver: ReceiverInfo {
                name: "Unifying Receiver".to_string(),
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                unique_id,
            },
            paired,
        }),
        healthy,
        outcomes,
    }
}

/// Probe a single Bolt pairing slot. Returns `None` when the slot is empty or
/// unreadable, otherwise the device plus its cache contribution this tick.
async fn probe_bolt_slot(
    channel: &Arc<HidppChannel>,
    bolt: &BoltReceiver,
    event: Option<&BoltDeviceConnection>,
    slot: u8,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> Option<(PairedDevice, CacheOutcome)> {
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
    let cached = id.as_ref().and_then(|i| cache.get(i));
    let register_kind = map_kind(bolt_kind);

    // Cap the feature walk per slot so one device that stops answering can't
    // burn the whole receiver's `PROBE_BUDGET` and time out `probe_one` — which
    // would drop *every* device on the receiver. A timed-out slot falls back to
    // its cached probe (its pairing-register identity above already read fine),
    // mirroring the Unifying path (#218).
    let probe_result = timeout(
        BOLT_SLOT_PROBE,
        probe_or_reuse(channel, slot, id.clone(), cached, online, tick),
    )
    .await;
    let (probe, outcome) = if let Ok(r) = probe_result {
        r
    } else {
        debug!(slot, budget = ?BOLT_SLOT_PROBE,
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
        codename,
        wpid,
        // Prefer the device's own `0x0005` type; the register kind is the
        // offline fallback.
        kind: resolve_device_kind(probe.kind, register_kind),
        online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    };
    Some((device, outcome))
}

/// Probe a HID++ channel that doesn't host a Bolt receiver — for
/// Bluetooth-direct, USB-C, or otherwise wired devices that present
/// themselves as a HID++ device rather than a receiver (P2.4).
///
/// Addresses the device at index `0xff` (HID++'s "self" slot) and reads
/// the same battery + model-info features the Bolt path uses. Yields no
/// inventory when the channel doesn't respond to HID++ at `0xff` (in which
/// case it's neither a receiver nor a direct device we recognise) — healthy
/// only if that rejection rests on a completed feature walk, so a device
/// that merely failed to answer is settled as a failed probe instead.
async fn probe_direct(
    channel: Arc<HidppChannel>,
    info: &async_hid::DeviceInfo,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> NodeProbe {
    let id = CacheKey::Direct(info.id.clone());
    let cached = cache.get(&id);
    // A direct device is always "present" (its HID node is the candidate), so
    // treat it as online: reuse the cached probe while fresh, otherwise probe.
    let (probe, outcome) =
        probe_or_reuse(&channel, DIRECT_DEVICE_INDEX, Some(id), cached, true, tick).await;
    // Hybrid peripheral discriminator. A genuine directly-attached device is
    // either wireless/Bluetooth — which reports a battery — or exposes a
    // configuration feature (buttons / pointer / lighting). A Bolt receiver's
    // secondary HID interface also answers DeviceInformation at 0xff, but
    // exposes neither battery nor those features, so it's filtered out here.
    // Without this guard a Bolt setup ends up with two entries in `device_list`:
    // the real mouse (via the Bolt path) and a phantom "direct device" pointing
    // at the receiver, which sits at index 0 and steals every DPI / SmartShift
    // write attempt. We reuse the capabilities the probe already derived from
    // the feature table — no extra round-trip.
    // A completed feature-table walk is what makes this probe's verdict
    // trustworthy: without it (the device never answered) a rejection below
    // would be indistinguishable from a transient glitch, so the node is
    // settled as a failed probe and its last inventory replayed.
    let capabilities = probe.capabilities;
    let walk_succeeded = capabilities.is_some();
    let caps = capabilities.unwrap_or_default();
    let is_peripheral = probe.battery.is_some() || caps.buttons || caps.pointer || caps.lighting;
    if !is_peripheral {
        debug!(
            vid = format_args!("{:04x}", info.vendor_id),
            pid = format_args!("{:04x}", info.product_id),
            has_model = probe.model_info.is_some(),
            "slot 0xff exposes no battery or config feature — likely a receiver \
             secondary interface; skipping"
        );
        // Don't cache or keep a rejected non-peripheral — `Unkeyed` lets any
        // prior entry for this node be evicted.
        return NodeProbe {
            inventory: None,
            healthy: walk_succeeded,
            outcomes: vec![CacheOutcome::Unkeyed],
        };
    }

    // Without a Bolt receiver we don't have a wpid, codename, or pairing
    // info — those live on the receiver registers. Use the HID name as
    // the display fallback and leave wpid empty.
    debug!(name = %info.name, "BT-direct / wired device recognised");
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: info.name.clone(),
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some(info.name.clone()),
            wpid: None,
            // No receiver pairing register here, so `0x0005` is the only kind
            // hint — but kind is just identity now; the UI gates on the
            // capabilities below, so a misread kind can't hide the panels (#127).
            kind: resolve_device_kind(probe.kind, DeviceKind::Unknown),
            online: true,
            battery: probe.battery,
            model_info: probe.model_info,
            capabilities,
        }],
    };
    NodeProbe {
        inventory: Some(inventory),
        healthy: true,
        outcomes: vec![outcome],
    }
}

async fn drain_device_arrival(bolt: &BoltReceiver) -> Vec<BoltDeviceConnection> {
    let rx = bolt.listen();
    if let Err(e) = bolt.trigger_device_arrival().await {
        debug!(error = ?e, "trigger_device_arrival failed; receiver may report no devices");
        return Vec::new();
    }

    let mut out = Vec::new();
    loop {
        match timeout(ARRIVAL_DRAIN, rx.recv()).await {
            Ok(Ok(BoltEvent::DeviceConnection(c))) => out.push(c),
            Ok(Ok(_)) => {} // BoltEvent is non_exhaustive; ignore future variants
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// `None` when the arrival trigger itself failed: unlike Bolt (whose paired
/// list comes from the slot registers), the drain is the only Unifying device
/// source, so the caller must treat that as a failed probe rather than an
/// empty receiver.
async fn drain_device_arrival_unifying(
    unifying: &UnifyingReceiver,
) -> Option<Vec<UnifyingDeviceConnection>> {
    let rx = unifying.listen();
    if let Err(e) = unifying.trigger_device_arrival().await {
        debug!(error = ?e, "trigger_device_arrival failed; receiver may report no devices");
        return None;
    }

    let mut out = Vec::new();
    loop {
        match timeout(ARRIVAL_DRAIN, rx.recv()).await {
            Ok(Ok(UnifyingEvent::DeviceConnection(c))) => out.push(c),
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    Some(out)
}

/// Probe a Unifying slot from a live device-connection event.
///
/// Device-arrival events carry the slot index, kind, wpid, and online status —
/// enough to surface an entry for every currently-connected device. The
/// unit_id (needed for stable caching across ticks) is not available without a
/// working `get_device_pairing_information` call; we derive a stable cache key
/// from the receiver UID + slot so the feature-table walk is amortised at ~30s
/// and two receivers sharing a slot number don't collide in the cache.
async fn probe_unifying_slot(
    channel: &Arc<HidppChannel>,
    event: &UnifyingDeviceConnection,
    receiver_uid: &str,
    cache: &HashMap<CacheKey, Cached>,
    tick: u64,
) -> Option<(PairedDevice, CacheOutcome)> {
    let slot = event.index;
    let codename = read_codename(channel, slot).await;
    debug!(
        slot,
        online = event.online,
        wpid = format_args!("{:04x}", event.wpid),
        kind = ?event.kind,
        codename = ?codename,
        "unifying paired slot"
    );

    // Cache key: full receiver serial + slot so two Unifying receivers with
    // a device on the same slot number never share a cache entry.
    let id = CacheKey::UnifyingSlot {
        receiver_uid: receiver_uid.to_string(),
        slot,
    };
    let cached = cache.get(&id);
    let register_kind = map_unifying_kind(event.kind);

    let probe_result = timeout(
        UNIFYING_SLOT_PROBE,
        probe_or_reuse(channel, slot, Some(id.clone()), cached, event.online, tick),
    )
    .await;
    let (probe, outcome) = if let Ok(r) = probe_result {
        r
    } else {
        debug!(slot, budget = ?UNIFYING_SLOT_PROBE,
            "Unifying slot probe timed out; using cached data if available");
        let probe = cached.map_or_else(ProbedFeatures::default, |c| c.probe.clone());
        (probe, CacheOutcome::Seen(id))
    };

    let device = PairedDevice {
        slot,
        codename,
        wpid: Some(event.wpid),
        kind: resolve_device_kind(probe.kind, register_kind),
        online: event.online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    };
    Some((device, outcome))
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
        .read_long_register(0xFF, 0xB5, [0x60 + slot, 0x01, 0x00])
        .await
        .ok()?;
    let len = usize::from(response[2]).min(13);
    core::str::from_utf8(&response[3..3 + len])
        .ok()
        .map(str::to_string)
}
