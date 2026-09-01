use std::{collections::HashMap, sync::Arc, time::Instant};

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

use super::events::EventSubscriptionHandle;
use super::mappings::{map_kind, map_unifying_kind, resolve_device_kind};
use crate::backend::NodeInfo;
use crate::channel::route::DIRECT_DEVICE_INDEX;

use super::cache::{CacheKey, CacheOutcome, Cached, is_stale, probe_or_reuse, seen};
use super::features::ProbedFeatures;
use super::{
    ARRIVAL_DRAIN, BOLT_SLOT_PROBE, MAX_BOLT_SLOTS, UNIFYING_CACHED_SLOT_PROBE, UNIFYING_SLOT_PROBE,
};

/// One node probe's verdict about its own trustworthiness. Three-valued on
/// purpose: the old `healthy`/`complete` bool pair could also express
/// "couldn't check, but the check is complete", which no probe path means —
/// the invariant lived in a comment at every construction site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProbeVerdict {
    /// The node could not be checked (budget timeout, unanswered registers, a
    /// feature walk that never finished): the ledger replays the last-good
    /// snapshot instead of presenting the failure as truth.
    Failed,
    /// The node answered — the only verdict that counts as stability
    /// evidence. `complete` reports whether every expected device was seen,
    /// which is what lets the one-shot retry stop early.
    Healthy {
        /// Every expected device is present in this probe's inventory.
        complete: bool,
    },
}

impl ProbeVerdict {
    /// `Healthy` with `complete` decided by the walk, `Failed` otherwise —
    /// for paths where one flag carries both facts.
    pub(super) fn healthy_when(answered_in_full: bool) -> Self {
        if answered_in_full {
            Self::Healthy { complete: true }
        } else {
            Self::Failed
        }
    }

    /// The node answered this tick (the ledger's replay gate).
    pub(super) fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    /// Every expected device was seen (the one-shot retry's stop signal).
    pub(super) fn is_complete(self) -> bool {
        matches!(self, Self::Healthy { complete: true })
    }
}

/// One probed node's contribution this tick: its inventory (if any), the
/// [`ProbeVerdict`] the ledger and the one-shot retry act on (see
/// [`super::ledger::NodeLedger::settle`]), and each device's cache
/// contribution for the caller to apply and to drive eviction.
pub(super) struct NodeProbe {
    pub(super) inventory: Option<DeviceInventory>,
    pub(super) verdict: ProbeVerdict,
    pub(super) outcomes: Vec<CacheOutcome>,
}

impl NodeProbe {
    /// A probe that got no answer at all (budget timeout).
    pub(super) fn failed() -> Self {
        Self {
            inventory: None,
            verdict: ProbeVerdict::Failed,
            outcomes: Vec::new(),
        }
    }
}

/// Probe one open HID++ node (channel reused across ticks by the caller).
pub(super) async fn probe_one(
    info: NodeInfo,
    channel: Arc<HidppChannel>,
    cache: &HashMap<CacheKey, Cached>,
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> NodeProbe {
    match receiver::detect(Arc::clone(&channel)) {
        Some(Receiver::Bolt(bolt)) => {
            probe_bolt_receiver(channel, info, bolt, cache, now, subscriptions).await
        }
        Some(Receiver::Unifying(unifying)) => {
            probe_unifying_receiver(channel, info, unifying, cache, now, subscriptions).await
        }
        None | Some(_) => {
            // No recognised receiver — this might be a directly-paired device
            // (Bluetooth-direct, USB-C cable). HID++ at device-index 0xff
            // addresses the device's own features. Probe in case it answers.
            // P2.4 — verified path; no Bolt-pairing slot indirection needed.
            probe_direct(channel, &info, cache, now, subscriptions).await
        }
    }
}

async fn probe_bolt_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    bolt: BoltReceiver,
    cache: &HashMap<CacheKey, Cached>,
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> NodeProbe {
    let unique_id = bolt.get_unique_id().await.ok();
    let pairing_count = bolt.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    let connections = drain_device_arrival(&bolt, subscriptions).await;
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

    // Phase 2 — walk each occupied slot's feature table concurrently. Every walk
    // addresses its own device index, so responses route by index (no
    // cross-talk), and this per-device walk is the slow part a laggy device
    // would otherwise serialize the rest of the receiver behind. Each is bounded
    // independently by `BOLT_SLOT_PROBE`; the ordered identity list keeps the
    // device list stable across ticks without an explicit sort.
    let slot_results = identities
        .iter()
        .map(|identity| walk_bolt_slot(&channel, identity, cache, now, subscriptions))
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
    let answered_in_full = pairing_count.is_some_and(|count| paired.len() == usize::from(count));

    NodeProbe {
        inventory: Some(DeviceInventory { receiver, paired }),
        verdict: ProbeVerdict::healthy_when(answered_in_full),
        outcomes,
    }
}

async fn probe_unifying_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    unifying: UnifyingReceiver,
    cache: &HashMap<CacheKey, Cached>,
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
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

    // Trigger device-arrival events and collect one event per paired slot.
    // Each event carries the slot index, kind, wpid, and a link-status bit —
    // enough to build a PairedDevice entry, online or not.
    //
    // Note: the Unifying `0xB5/0x5N` pairing-info register uses a different
    // sub-register base than Bolt, so paired slots are not polled directly.
    // A slot whose re-broadcast goes missing this tick cannot be backfilled
    // until that register format is resolved.
    //
    // The drain is therefore the *only* device source on this path, so a
    // failed arrival trigger is "couldn't check", not "no devices online":
    // settle it as a failed probe and let the ledger replay the last snapshot.
    let Some(connections) =
        drain_device_arrival_unifying(&unifying, pairing_count, subscriptions).await
    else {
        return NodeProbe::failed();
    };
    debug!(events = connections.len(), "drained device-arrival events");

    // The receiver can re-broadcast the same 0x41 for a slot more than once per
    // trigger, so keep one connection per slot — otherwise the device is listed
    // twice. Last write wins: a later event carries the freshest online flag.
    let mut connections: Vec<_> = connections
        .into_iter()
        .map(|c| (c.index, c))
        .collect::<HashMap<_, _>>()
        .into_values()
        .collect();
    // HashMap iteration is unordered; sort by slot so the device list is stable
    // across probe cycles instead of jittering.
    connections.sort_by_key(|c| c.index);

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
        .map(|conn| probe_unifying_slot(&channel, conn, receiver_uid, cache, now, subscriptions))
        .collect::<Vec<_>>()
        .join()
        .await;

    let (paired, outcomes): (Vec<_>, Vec<_>) = slot_results.into_iter().flatten().unzip();

    if paired.len() != usize::from(pairing_count) {
        debug!(
            expected = pairing_count,
            found = paired.len(),
            "arrival drain reported fewer slots than the pairing count"
        );
    }
    // Unlike Bolt, a count/list shortfall is tolerated here: not every
    // firmware re-broadcasts all paired slots (offline slots in particular can
    // go missing), and there is no register poll to backfill them, so ledger
    // health can't ride on it. The
    // ledger health signal is the pairing-count register answering at all: that
    // proves the receiver round-trip worked this cycle, while `None` (e.g. a
    // parked channel) is "couldn't fully check" — the ledger then replays the
    // last good snapshot instead of presenting a possibly-empty list (#218).
    //
    // The one-shot CLI path still needs a retry when the count says more
    // devices may appear after a late arrival drain. Report that separately as
    // `complete: false`; the unchanged-inventory fallback stops expected
    // offline Unifying shortfalls after they stabilize.
    let complete = paired.len() == usize::from(pairing_count);

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
        verdict: ProbeVerdict::Healthy { complete },
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
    cache: &HashMap<CacheKey, Cached>,
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
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
        probe_or_reuse(
            channel,
            slot,
            id.clone(),
            cached,
            online,
            now,
            subscriptions,
        ),
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
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> NodeProbe {
    let id = CacheKey::Direct(info.id.clone());
    let cached = cache.get(&id);
    // A direct device is always "present" (its HID node is the candidate), so
    // treat it as online: reuse the cached probe while fresh, otherwise probe.
    let (probe, outcome) = probe_or_reuse(
        &channel,
        DIRECT_DEVICE_INDEX,
        Some(id),
        cached,
        true,
        now,
        subscriptions,
    )
    .await;
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
            verdict: ProbeVerdict::Failed,
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
            verdict: ProbeVerdict::healthy_when(walk_succeeded),
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
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes: vec![outcome],
    }
}

async fn drain_device_arrival(
    bolt: &BoltReceiver,
    subscriptions: Option<&EventSubscriptionHandle>,
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
    subscriptions: Option<&EventSubscriptionHandle>,
) -> Option<Vec<UnifyingDeviceConnection>> {
    let rx = unifying.listen();
    let _receiver_snapshot = subscriptions.map(EventSubscriptionHandle::begin_receiver_snapshot);
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
    // Keep unsolicited lifecycle notifications enabled after the triggered
    // snapshot. This is read-modify-write and a no-op when already enabled.
    let notification_result = unifying.set_wireless_notifications(true).await;
    // A receiver with no pairings legitimately emits nothing: don't pay a
    // second drain window for it on every reconciliation.
    if !out.is_empty() || pairing_count == 0 {
        if let Err(error) = notification_result {
            debug!(?error, "enable persistent wireless notifications failed");
        }
        return Some(out);
    }

    // Classic Unifying receivers only re-broadcast 0x41 arrival events while
    // wireless notifications are on. Fall back to enabling that flag when the
    // direct trigger produced no device, then retry once on the same listener.
    if let Err(error) = notification_result {
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

/// Probe a Unifying slot from a live device-connection event.
///
/// Device-arrival events carry the slot index, kind, wpid, and online status —
/// enough to surface an entry for every currently-connected device. The
/// unit_id (needed for stable caching across ticks) is not available without a
/// working `get_device_pairing_information` call; we derive a stable cache key
/// from the receiver UID + slot so the feature-table walk is amortised at ~30s
/// and two receivers sharing a slot number don't collide in the cache.
pub(super) async fn probe_unifying_slot(
    channel: &Arc<HidppChannel>,
    event: &UnifyingDeviceConnection,
    receiver_uid: &str,
    cache: &HashMap<CacheKey, Cached>,
    now: Instant,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> Option<(PairedDevice, CacheOutcome)> {
    let slot = event.index;
    // Cache key: full receiver serial + slot so two Unifying receivers with
    // a device on the same slot number never share a cache entry.
    let id = CacheKey::UnifyingSlot {
        receiver_uid: receiver_uid.to_string(),
        slot,
    };
    let cached = cache.get(&id);
    let register_kind = map_unifying_kind(event.kind);

    // The 0x41 re-broadcast is the receiver's own slot report and its
    // link-status bit is the liveness authority (Solaar's trigger scan trusts
    // the same bit). The feature/battery refresh below is optional metadata:
    // keep it bounded, never let its one lost reply turn a device that just
    // announced itself into "offline" — and don't probe an offline slot at
    // all, which would burn the budget on a link the receiver just reported
    // as not established.
    let probe_budget = unifying_probe_budget(cached, now);
    let probe_result = timeout(
        probe_budget,
        probe_or_reuse(
            channel,
            slot,
            Some(id.clone()),
            cached,
            event.online,
            now,
            subscriptions,
        ),
    )
    .await;
    let (probe, outcome) = if let Ok(result) = probe_result {
        result
    } else {
        debug!(slot, budget = ?probe_budget,
            "Unifying slot probe timed out; using cached data if available");
        let probe = cached.map_or_else(ProbedFeatures::default, |entry| entry.probe.clone());
        (probe, CacheOutcome::Seen(id))
    };

    // HID++ 2.0's marketing name is the same identity we need for display and
    // avoids another receiver-register round trip. Keep the legacy codename
    // read only for a completed feature walk that did not expose a name; never
    // put it in front of the feature probe, where one missing receiver ACK can
    // otherwise starve a healthy Lightspeed mouse forever.
    let codename = if let Some(name) = probe.marketing_name.clone() {
        Some(name)
    } else if probe.capabilities.is_some() {
        read_codename_unifying(channel, slot).await
    } else {
        None
    };
    debug!(
        slot,
        online = event.online,
        wpid = format_args!("{:04x}", event.wpid),
        kind = ?event.kind,
        codename = ?codename,
        "unifying paired slot"
    );

    let device = assemble_unifying_device(
        slot,
        codename,
        event.wpid,
        register_kind,
        probe,
        event.online,
    );
    Some((device, outcome))
}

/// A fresh cache hit needs only an optional battery refresh; first-sight and
/// stale entries retain the larger budget needed for a complete feature walk.
pub(super) fn unifying_probe_budget(cached: Option<&Cached>, now: Instant) -> std::time::Duration {
    if cached.is_some_and(|entry| !is_stale(entry, now)) {
        UNIFYING_CACHED_SLOT_PROBE
    } else {
        UNIFYING_SLOT_PROBE
    }
}

pub(super) fn assemble_unifying_device(
    slot: u8,
    codename: Option<String>,
    wpid: u16,
    register_kind: DeviceKind,
    probe: ProbedFeatures,
    online: bool,
) -> PairedDevice {
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
