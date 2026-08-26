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
use openlogi_core::device::{
    DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice, ReceiverInfo,
};
use tokio::time::timeout;
use tracing::{debug, warn};

use super::mappings::{map_kind, map_unifying_kind, resolve_device_kind};
use crate::backend::NodeInfo;
use crate::channel::route::DIRECT_DEVICE_INDEX;

use super::cache::{CacheKey, CacheOutcome, Cached, probe_or_reuse, seen};
use super::features::ProbedFeatures;
use super::{
    ARRIVAL_DRAIN, BOLT_SLOT_PROBE, MAX_RECEIVER_SLOTS, UNIFYING_CACHED_SLOT_PROBE,
    UNIFYING_SLOT_PROBE,
};

/// One probed node's contribution this tick: its inventory (if any), whether
/// the node actually answered — the ledger replays the last snapshot when it
/// didn't (see [`super::ledger::NodeLedger::settle`]) — whether the
/// one-shot retry can stop early, and each device's cache contribution for the
/// caller to apply and to drive eviction.
pub(super) struct NodeProbe {
    pub(super) inventory: Option<DeviceInventory>,
    pub(super) healthy: bool,
    pub(super) complete: bool,
    pub(super) outcomes: Vec<CacheOutcome>,
}

impl NodeProbe {
    /// A probe that got no answer at all (budget timeout).
    pub(super) fn failed() -> Self {
        Self {
            inventory: None,
            healthy: false,
            complete: false,
            outcomes: Vec::new(),
        }
    }
}

/// Probe one open HID++ node (channel reused across ticks by the caller).
pub(super) async fn probe_one(
    info: NodeInfo,
    channel: Arc<HidppChannel>,
    cache: &HashMap<CacheKey, Cached>,
) -> NodeProbe {
    match receiver::detect(Arc::clone(&channel)) {
        Some(Receiver::Bolt(bolt)) => probe_bolt_receiver(channel, info, bolt, cache).await,
        Some(Receiver::Unifying(unifying)) => {
            probe_unifying_receiver(channel, info, unifying, cache).await
        }
        None | Some(_) => {
            // No recognised receiver — this might be a directly-paired device
            // (Bluetooth-direct, USB-C cable). HID++ at device-index 0xff
            // addresses the device's own features. Probe in case it answers.
            // P2.4 — verified path; no Bolt-pairing slot indirection needed.
            probe_direct(channel, &info, cache).await
        }
    }
}

async fn probe_bolt_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    bolt: BoltReceiver,
    cache: &HashMap<CacheKey, Cached>,
) -> NodeProbe {
    let unique_id = bolt.get_unique_id().await.ok();
    let pairing_count = bolt.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    let connections = drain_device_arrival(&bolt).await;
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
    for slot in 1u8..=MAX_RECEIVER_SLOTS {
        if let Some(identity) =
            read_bolt_slot_identity(&bolt, &channel, by_slot.get(&slot), slot).await
        {
            identities.push(identity);
        }
    }

    // Phase 2 — walk each occupied slot's feature table concurrently. Every walk
    // addresses its own device index, so responses route by index (no
    // cross-talk), and this per-device walk is the slow part a laggy device
    // would otherwise serialize the rest of the receiver behind. Each is bounded
    // independently by `BOLT_SLOT_PROBE`; the ordered identity list keeps the
    // device list stable across ticks without an explicit sort.
    let slot_results = identities
        .iter()
        .map(|identity| walk_bolt_slot(&channel, identity, cache))
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
/// never reach here. The probe is `complete`/`healthy` only when the
/// pairing-count register answered AND every counted slot was readable: `None`
/// (the receiver didn't answer, e.g. a parked channel) or a shortfall is
/// "couldn't fully check", so the ledger replays the last good snapshot instead
/// of presenting the partial walk as the new truth (#218). A slot whose feature
/// walk merely timed out still counts here — it falls back to cached/identity
/// data in [`walk_bolt_slot`].
pub(super) fn assemble_bolt_probe(
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
    let complete = pairing_count.is_some_and(|count| paired.len() == usize::from(count));

    NodeProbe {
        inventory: Some(DeviceInventory { receiver, paired }),
        healthy: complete,
        complete,
        outcomes,
    }
}

async fn probe_unifying_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    unifying: UnifyingReceiver,
    cache: &HashMap<CacheKey, Cached>,
) -> NodeProbe {
    // Pairing count is the health gate for this path: without it the result is
    // settled as a failed probe regardless of any later arrival events. Check
    // it first and stop immediately on failure instead of spending two more
    // request timeouts enabling notifications and triggering arrivals on a
    // channel that has already stopped delivering receiver replies.
    let pairing_count = match unifying.count_pairings().await {
        Ok(count) => count,
        Err(error) => {
            debug!(?error, "receiver pairing-count read failed");
            return NodeProbe::failed();
        }
    };
    debug!(pairing_count, "receiver reports pairing count");
    let unique_id = unifying.get_unique_id().await.ok();

    // Arrival events remain the liveness authority, but they are not an
    // inventory authority: a sleeping device often emits no event at all.
    // Read every occupied slot from the receiver registers as the durable
    // inventory, then overlay the freshest event's online bit when present.
    let arrivals = drain_device_arrival_unifying(&unifying, pairing_count).await;
    let arrival_ok = arrivals.is_some();
    let by_slot: HashMap<u8, UnifyingDeviceConnection> = arrivals
        .unwrap_or_default()
        .into_iter()
        .map(|connection| (connection.index, connection))
        .collect();
    debug!(events = by_slot.len(), "drained device-arrival events");

    // Pairing and extended-pairing reads all address receiver register 0xB5.
    // The channel correlates those replies by register rather than by the slot
    // encoded in the payload, so issue them sequentially. Extended pairing
    // supplies the physical unit id that lets receiver and Bluetooth routes
    // resolve to one device without ever merging two same-model units.
    let mut identities = Vec::new();
    for slot in 1u8..=MAX_RECEIVER_SLOTS {
        if let Some(identity) =
            read_unifying_slot_identity(&unifying, by_slot.get(&slot), slot).await
        {
            identities.push(identity);
            if identities.len() == usize::from(pairing_count) {
                break;
            }
        }
    }

    // Feature walks address separate device indexes and are safe to run in
    // parallel. A sleeping slot skips I/O and still surfaces with its stable
    // receiver-stored identity.
    let mut slot_results = identities
        .iter()
        .map(|identity| walk_unifying_slot(&channel, identity, cache))
        .collect::<Vec<_>>()
        .join()
        .await;

    // Legacy receiver codenames also share register 0xB5. Preserve the old
    // fallback, but serialize it after feature walks so one missing receiver
    // ACK cannot block a healthy HID++ 2.0 device from being identified.
    for (device, _) in &mut slot_results {
        if device.codename.is_none() && device.capabilities.is_some() {
            device.codename = read_codename_unifying(&channel, device.slot).await;
        }
    }

    let (paired, outcomes): (Vec<_>, Vec<_>) = slot_results.into_iter().unzip();

    if paired.len() != usize::from(pairing_count) {
        debug!(
            expected = pairing_count,
            found = paired.len(),
            "pairing registers reported fewer slots than the pairing count"
        );
    }
    let complete = paired.len() == usize::from(pairing_count);
    // A missing arrival trigger means online state was not checked. Publish
    // the register inventory as a fallback only after the ledger's normal
    // failure grace; until then replay the last-good liveness snapshot.
    let healthy = complete && arrival_ok;

    NodeProbe {
        inventory: Some(DeviceInventory {
            receiver: ReceiverInfo {
                name: crate::channel::route::receiver_display_name(info.product_id).to_string(),
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                unique_id,
            },
            paired,
        }),
        healthy,
        complete,
        outcomes,
    }
}

/// Receiver-stored identity for one occupied Unifying slot.
///
/// The unit id is physical-device identity; slot and WPID are only route/model
/// metadata. An all-zero unit id remains unkeyed and is never cached.
pub(super) struct UnifyingSlotIdentity {
    pub(super) slot: u8,
    pub(super) id: Option<CacheKey>,
    pub(super) unit_id: [u8; 4],
    pub(super) online: bool,
    pub(super) register_kind: DeviceKind,
    pub(super) wpid: u16,
}

async fn read_unifying_slot_identity(
    unifying: &UnifyingReceiver,
    event: Option<&UnifyingDeviceConnection>,
    slot: u8,
) -> Option<UnifyingSlotIdentity> {
    let pairing = match unifying.get_device_pairing_information(slot).await {
        Ok(pairing) => pairing,
        Err(error) => {
            debug!(slot, ?error, "Unifying slot empty or unreadable");
            return None;
        }
    };
    let unit_id = match unifying.get_device_extended_pairing_information(slot).await {
        Ok(extended) => extended.unit_id,
        Err(error) => {
            debug!(slot, ?error, "Unifying extended identity unavailable");
            [0; 4]
        }
    };
    let online = event.is_some_and(|connection| connection.online);
    let register_kind = event.map_or_else(
        || map_unifying_kind(pairing.kind),
        |connection| map_unifying_kind(connection.kind),
    );
    let wpid = event.map_or(pairing.wpid, |connection| connection.wpid);
    let id = (unit_id != [0; 4]).then_some(CacheKey::Unifying { unit_id });
    debug!(
        slot,
        online,
        wpid = format_args!("{wpid:04x}"),
        ?register_kind,
        ?unit_id,
        has_event = event.is_some(),
        "Unifying paired slot"
    );
    Some(UnifyingSlotIdentity {
        slot,
        id,
        unit_id,
        online,
        register_kind,
        wpid,
    })
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
    cache: &HashMap<CacheKey, Cached>,
) -> (PairedDevice, CacheOutcome) {
    let &BoltSlotIdentity {
        slot,
        online,
        register_kind,
        wpid,
        ..
    } = identity;
    let id = identity.id.clone();
    let cached = id.as_ref().and_then(|i| cache.get(i));

    // Cap the feature walk per slot so one device that stops answering can't
    // burn the whole receiver's `PROBE_BUDGET` and time out `probe_one` — which
    // would drop *every* device on the receiver. A timed-out slot falls back to
    // its cached probe (its pairing-register identity read fine in phase 1),
    // mirroring the Unifying path (#218).
    let probe_result = timeout(
        BOLT_SLOT_PROBE,
        probe_or_reuse(channel, slot, id.clone(), cached, online),
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

/// Prefer the device's own HID++ marketing name over the host HID collection
/// label. Windows Bluetooth frequently exposes only a generic `"Mouse"`, while
/// feature `0x0005` carries the real model name (for example MX Master 2S).
pub(super) fn preferred_direct_codename(marketing_name: Option<&str>, os_name: &str) -> String {
    marketing_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(os_name)
        .to_string()
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
    info: &NodeInfo,
    cache: &HashMap<CacheKey, Cached>,
) -> NodeProbe {
    let id = CacheKey::Direct(info.id.clone());
    let cached = cache.get(&id);
    // A direct device is always "present" (its HID node is the candidate), so
    // treat it as online: reuse the cached probe while fresh, otherwise probe.
    let (probe, outcome) =
        probe_or_reuse(&channel, DIRECT_DEVICE_INDEX, Some(id), cached, true).await;
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
    // A walk that never completed says nothing about what this node is: the
    // discriminator below would read "no battery, no config feature" off an
    // empty probe and reject a real mouse as a receiver's secondary interface.
    // Settle it as a transient failure and keep the node's cache entry, so the
    // last-good inventory is replayed while the link recovers.
    if !walk_succeeded {
        debug!(
            vid = format_args!("{:04x}", info.vendor_id),
            pid = format_args!("{:04x}", info.product_id),
            "feature walk did not complete — transient probe failure, keeping last-known identity"
        );
        return NodeProbe {
            inventory: None,
            healthy: false,
            complete: false,
            outcomes: vec![seen(Some(CacheKey::Direct(info.id.clone())))],
        };
    }
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
            complete: walk_succeeded,
            outcomes: vec![CacheOutcome::Unkeyed],
        };
    }

    // Direct devices have no receiver codename register. Prefer the device's
    // own 0x0005 marketing name; the Windows Bluetooth HID collection often
    // calls every pointing device simply `"Mouse"`.
    let codename = preferred_direct_codename(probe.marketing_name.as_deref(), &info.name);
    debug!(os_name = %info.name, name = %codename, "BT-direct / wired device recognised");
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: info.name.clone(),
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some(codename),
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
        complete: true,
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

/// `None` when the receiver could not be asked: the arrival trigger failed,
/// or the notification-flag fallback write did. Unlike Bolt (whose paired
/// list comes from the slot registers), the drain is the only Unifying device
/// source, so the caller must treat that as a failed probe rather than an
/// empty receiver.
async fn drain_device_arrival_unifying(
    unifying: &UnifyingReceiver,
    pairing_count: u8,
) -> Option<Vec<UnifyingDeviceConnection>> {
    let rx = unifying.listen();
    // Newer Lightspeed receivers can already have notifications enabled (or
    // emit the requested arrival event without changing the legacy Unifying
    // flag). Ask first: c54d has been observed to answer this trigger while
    // occasionally withholding the ACK for the notification-register setup,
    // which otherwise stalls discovery before it reaches the useful request.
    if let Err(e) = unifying.trigger_device_arrival().await {
        debug!(error = ?e, "trigger_device_arrival failed; receiver may report no devices");
        return None;
    }
    let mut out = Vec::new();
    loop {
        match timeout(ARRIVAL_DRAIN, rx.recv()).await {
            Ok(Ok(UnifyingEvent::DeviceConnection(connection))) => out.push(connection),
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    // A receiver with no pairings legitimately emits nothing: don't pay a
    // notification-register round trip and a second drain window for it on
    // every watcher tick.
    if !out.is_empty() || pairing_count == 0 {
        return Some(out);
    }

    // Classic Unifying receivers only re-broadcast 0x41 arrival events while
    // wireless notifications are on. Fall back to enabling that flag when the
    // direct trigger produced no device, then retry once on the same listener.
    if let Err(error) = unifying.set_wireless_notifications(true).await {
        // A register write the receiver stopped ACK'ing is "couldn't check",
        // exactly like a failed trigger: settle it as a failed probe so the
        // ledger replays the last snapshot, instead of publishing an
        // authoritative empty inventory that overwrites the node's last-good
        // device list.
        debug!(?error, "enable wireless notifications failed");
        return None;
    }
    if let Err(error) = unifying.trigger_device_arrival().await {
        debug!(?error, "arrival retry after enabling notifications failed");
        return None;
    }
    out.clear();
    loop {
        match timeout(ARRIVAL_DRAIN, rx.recv()).await {
            Ok(Ok(UnifyingEvent::DeviceConnection(connection))) => out.push(connection),
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => return Some(out),
        }
    }
}

/// Walk one Unifying slot using the receiver-stored physical identity read in
/// phase 1. Sleeping devices perform no device I/O but still carry their unit
/// id into the inventory, allowing an offline receiver route to reconcile with
/// the same unit's Bluetooth route.
pub(super) async fn walk_unifying_slot(
    channel: &Arc<HidppChannel>,
    identity: &UnifyingSlotIdentity,
    cache: &HashMap<CacheKey, Cached>,
) -> (PairedDevice, CacheOutcome) {
    let slot = identity.slot;
    let id = identity.id.clone();
    let cached = id.as_ref().and_then(|key| cache.get(key));

    // The 0x41 re-broadcast is the receiver's own slot report and its
    // link-status bit is the liveness authority (Solaar's trigger scan trusts
    // the same bit). The feature/battery refresh below is optional metadata:
    // keep it bounded, never let its one lost reply turn a device that just
    // announced itself into "offline" — and don't probe an offline slot at
    // all, which would burn the budget on a link the receiver just reported
    // as not established.
    let probe_budget = unifying_probe_budget(cached, channel);
    let probe_result = timeout(
        probe_budget,
        probe_or_reuse(channel, slot, id.clone(), cached, identity.online),
    )
    .await;
    let (probe, outcome) = if let Ok(result) = probe_result {
        result
    } else {
        debug!(slot, budget = ?probe_budget,
            "Unifying slot probe timed out; using cached data if available");
        let probe = cached.map_or_else(ProbedFeatures::default, |entry| entry.probe.clone());
        (probe, seen(id))
    };

    let codename = probe.marketing_name.clone();
    debug!(
        slot,
        online = identity.online,
        wpid = format_args!("{:04x}", identity.wpid),
        kind = ?identity.register_kind,
        codename = ?codename,
        "unifying paired slot"
    );

    let device = assemble_unifying_device(
        slot,
        codename,
        identity.wpid,
        identity.unit_id,
        identity.register_kind,
        probe,
        identity.online,
    );
    (device, outcome)
}

/// A cache hit needs only an optional battery refresh; first sight retains the
/// larger budget needed for a complete feature walk. A cache bound to a
/// replaced channel also gets the full budget for its one validation walk.
pub(super) fn unifying_probe_budget(
    cached: Option<&Cached>,
    channel: &Arc<HidppChannel>,
) -> std::time::Duration {
    if cached.is_some_and(|entry| !entry.needs_validation(channel)) {
        UNIFYING_CACHED_SLOT_PROBE
    } else {
        UNIFYING_SLOT_PROBE
    }
}

pub(super) fn assemble_unifying_device(
    slot: u8,
    codename: Option<String>,
    wpid: u16,
    unit_id: [u8; 4],
    register_kind: DeviceKind,
    mut probe: ProbedFeatures,
    online: bool,
) -> PairedDevice {
    // The extended pairing register is readable even while the device sleeps
    // and is the same physical unit id HID++ 2.0 reports over Bluetooth. Fold
    // it into the model payload so downstream identity resolution can unify
    // routes without any model-name heuristic. A HID++ 1.0/offline device may
    // have no feature-table model info at all; synthesize only the fields the
    // receiver authoritatively owns.
    if unit_id != [0; 4] {
        if let Some(model) = probe.model_info.as_mut() {
            model.unit_id = unit_id;
        } else {
            probe.model_info = Some(DeviceModelInfo {
                entity_count: 0,
                serial_number: None,
                unit_id,
                transports: DeviceTransports {
                    equad: true,
                    ..DeviceTransports::default()
                },
                model_ids: [wpid, 0, 0],
                extended_model_id: 0,
            });
        }
    }
    PairedDevice {
        slot,
        codename,
        wpid: Some(wpid),
        kind: resolve_device_kind(probe.kind, register_kind),
        online,
        battery: probe.battery,
        model_info: probe.model_info,
        capabilities: probe.capabilities,
    }
}

/// Reads a Unifying paired device's name. Unifying stores names at
/// sub-register base `0x40` (device `n` at `0x40 + (n-1)`), a different layout
/// from Bolt's `0x60`: the long-register response is `[sub, len, data..]` with
/// no chunk byte — wire-verified `40 0c "MX Master 2S"`. The name lives on the
/// receiver, so it reads even while the device is offline (e.g. moved to BT).
async fn read_codename_unifying(channel: &HidppChannel, slot: u8) -> Option<String> {
    let response = channel
        .read_long_register(0xFF, 0xB5, [0x40 + slot - 1, 0x00, 0x00])
        .await
        .ok()?;
    parse_codename_unifying(&response)
}

/// Parse a Unifying name-register response `[sub, len, data..]` into a string.
/// The device-reported `len` is clamped to the bytes actually present so a
/// bogus length can't over-read the fixed long-register buffer.
pub(super) fn parse_codename_unifying(response: &[u8]) -> Option<String> {
    let len = usize::from(*response.get(1)?).min(response.len().saturating_sub(2));
    core::str::from_utf8(response.get(2..2 + len)?)
        .ok()
        .map(str::to_string)
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
