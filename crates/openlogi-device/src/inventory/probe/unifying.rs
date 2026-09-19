//! Probing a Unifying receiver: the device-arrival drain is its only device
//! list, so the register phase is held across the slot walks and a receiver that
//! answers its pairing count but not the arrival trigger is alive, not failed.

use std::{
    collections::HashMap,
    fmt::Debug,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_concurrency::future::Join as _;
use hidpp::{
    channel::HidppChannel,
    receiver::unifying::{
        DeviceConnection as UnifyingDeviceConnection, Event as UnifyingEvent,
        Receiver as UnifyingReceiver,
    },
};
use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use tokio::time::timeout;
use tracing::debug;

use super::{
    NodeProbe, PassContext, ProbeTimeouts, ProbeVerdict, RECEIVER_OPERATION_TIMEOUT,
    RECEIVER_UID_TIMEOUT, UNIFYING_TRIGGER_ATTEMPT_TIMEOUT, UNIFYING_TRIGGER_RETRY_DELAY,
};
use crate::backend::NodeInfo;
use crate::host_lock::ReceiverRegisterPhase;
use crate::inventory::cache::{CacheKey, CacheOutcome, Cached, is_stale, probe_or_reuse};
use crate::inventory::events::EventSubscriptionHandle;
use crate::inventory::features::ProbedFeatures;
use crate::inventory::mappings::{map_unifying_kind, resolve_device_kind};

/// Probe a Unifying receiver under its register phase, held to the end: a
/// slot whose feature walk exposes no marketing name reads its codename from
/// the receiver's `0xB5` register *after* the walk, and an error reply to
/// one `0xB5` sub-register is indistinguishable from another's, so the
/// phase cannot be released before the last register read the probe may
/// make. Unlike Bolt, then, the slot walks run under it — a few seconds at
/// most, which [`host_lock::RECEIVER_REGISTER_TIMEOUT`] allows for.
///
/// [`host_lock::RECEIVER_REGISTER_TIMEOUT`]: crate::host_lock::RECEIVER_REGISTER_TIMEOUT
pub(super) async fn probe_unifying_receiver(
    channel: Arc<HidppChannel>,
    info: NodeInfo,
    unifying: UnifyingReceiver,
    registers: ReceiverRegisterPhase,
    pass: PassContext<'_>,
) -> NodeProbe {
    // Pairing count is the health gate for this path: without it the result is
    // settled as a failed probe regardless of any later arrival events. Check
    // it first and stop immediately on failure instead of spending two more
    // request timeouts enabling notifications and triggering arrivals on a
    // channel that has already stopped delivering receiver replies.
    let pairing_count = match timeout(RECEIVER_OPERATION_TIMEOUT, unifying.count_pairings()).await {
        Ok(Ok(count)) => count,
        Ok(Err(error)) => {
            debug!(?error, "receiver pairing-count read failed");
            return NodeProbe::failed();
        }
        Err(_) => {
            debug!(
                budget = ?RECEIVER_OPERATION_TIMEOUT,
                "receiver pairing-count read timed out"
            );
            return NodeProbe::failed();
        }
    };
    debug!(pairing_count, "receiver reports pairing count");
    let unique_id = timeout(RECEIVER_UID_TIMEOUT, unifying.get_unique_id())
        .await
        .ok()
        .and_then(Result::ok);

    // Trigger device-arrival events and collect one event per paired slot.
    // Each event carries the slot index, kind, wpid, and a link-status bit —
    // enough to build a PairedDevice entry, online or not.
    //
    // Note: the Unifying `0xB5/0x5N` pairing-info register uses a different
    // sub-register base than Bolt, so paired slots are not polled directly.
    // A slot whose re-broadcast goes missing this tick cannot be backfilled
    // until that register format is resolved.
    //
    // The drain is therefore the only source of a fresh device list. A failed
    // trigger leaves that list unchanged, but the successful pairing-count
    // read above proves the receiver channel is still live; don't tear down a
    // working capture session for this narrower transient.
    let Some(connections) = drain_device_arrival_unifying(
        &unifying,
        pairing_count,
        pass.subscriptions,
        pass.timeouts.arrival_drain,
    )
    .await
    else {
        return NodeProbe::arrival_replay_failed();
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
    // one device doesn't push the next slot past the PROBE_TIMEOUT deadline.
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
        .map(|conn| probe_unifying_slot(&channel, conn, receiver_uid, pass))
        .collect::<Vec<_>>()
        .join()
        .await;
    // The last register read this probe can make — a slot's codename — is
    // behind the walks, so the phase is released only here.
    drop(registers);

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

/// `None` when the receiver could not be asked: the arrival trigger failed,
/// or the notification-flag fallback write did. Unlike Bolt (whose paired
/// list comes from the slot registers), the drain is the only Unifying device
/// source, so the caller must treat that as a failed probe rather than an
/// empty receiver.
async fn drain_device_arrival_unifying(
    unifying: &UnifyingReceiver,
    pairing_count: u8,
    subscriptions: Option<&EventSubscriptionHandle>,
    drain: Duration,
) -> Option<Vec<UnifyingDeviceConnection>> {
    let rx = unifying.listen();
    let _receiver_snapshot = subscriptions.map(EventSubscriptionHandle::begin_receiver_snapshot);
    // Newer Lightspeed receivers can already have notifications enabled (or
    // emit the requested arrival event without changing the legacy Unifying
    // flag). Ask first: c54d has been observed to answer this trigger while
    // occasionally withholding the ACK for the notification-register setup,
    // which otherwise stalls discovery before it reaches the useful request.
    retry_arrival_trigger(
        || unifying.trigger_device_arrival(),
        UNIFYING_TRIGGER_ATTEMPT_TIMEOUT,
        UNIFYING_TRIGGER_RETRY_DELAY,
    )
    .await?;
    let mut out = Vec::new();
    loop {
        match timeout(drain, rx.recv()).await {
            Ok(Ok(UnifyingEvent::DeviceConnection(connection))) => out.push(connection),
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    // Keep unsolicited lifecycle notifications enabled after the triggered
    // snapshot. This is read-modify-write and a no-op when already enabled.
    let notification_result = timeout(
        RECEIVER_OPERATION_TIMEOUT,
        unifying.set_wireless_notifications(true),
    )
    .await
    .map_err(|_| ())
    .and_then(|result| result.map_err(|_| ()));
    // A receiver with no pairings legitimately emits nothing: don't pay a
    // second drain window for it on every reconciliation.
    if !out.is_empty() || pairing_count == 0 {
        if notification_result.is_err() {
            debug!("enable persistent wireless notifications failed");
        }
        return Some(out);
    }

    // Classic Unifying receivers only re-broadcast 0x41 arrival events while
    // wireless notifications are on. Fall back to enabling that flag when the
    // direct trigger produced no device, then retry once on the same listener.
    if notification_result.is_err() {
        // A register write the receiver stopped ACK'ing is "couldn't check",
        // exactly like a failed trigger: settle it as a failed probe so the
        // ledger replays the last snapshot, instead of publishing an
        // authoritative empty inventory that overwrites the node's last-good
        // device list.
        debug!("enable wireless notifications failed");
        return None;
    }
    retry_arrival_trigger(
        || unifying.trigger_device_arrival(),
        UNIFYING_TRIGGER_ATTEMPT_TIMEOUT,
        UNIFYING_TRIGGER_RETRY_DELAY,
    )
    .await?;
    out.clear();
    loop {
        match timeout(drain, rx.recv()).await {
            Ok(Ok(UnifyingEvent::DeviceConnection(connection))) => out.push(connection),
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => return Some(out),
        }
    }
}

/// Retry one transient receiver refusal without hiding persistent failures
/// from the inventory ledger.
pub(in crate::inventory) async fn retry_arrival_trigger<F, Fut, E>(
    mut trigger: F,
    attempt_timeout: Duration,
    retry_delay: Duration,
) -> Option<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: Debug,
{
    match timeout(attempt_timeout, trigger()).await {
        Ok(Ok(())) => return Some(()),
        Ok(Err(error)) => debug!(?error, "trigger_device_arrival failed; retrying once"),
        Err(_) => debug!(
            ?attempt_timeout,
            "trigger_device_arrival timed out; retrying once"
        ),
    }
    tokio::time::sleep(retry_delay).await;
    match timeout(attempt_timeout, trigger()).await {
        Ok(Ok(())) => Some(()),
        Ok(Err(error)) => {
            debug!(
                ?error,
                "trigger_device_arrival retry failed; receiver may report no devices"
            );
            None
        }
        Err(_) => {
            debug!(
                ?attempt_timeout,
                "trigger_device_arrival retry timed out; receiver may report no devices"
            );
            None
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
pub(in crate::inventory) async fn probe_unifying_slot(
    channel: &Arc<HidppChannel>,
    event: &UnifyingDeviceConnection,
    receiver_uid: &str,
    pass: PassContext<'_>,
) -> Option<(PairedDevice, CacheOutcome)> {
    let slot = event.index;
    // Cache key: full receiver serial + slot so two Unifying receivers with
    // a device on the same slot number never share a cache entry.
    let id = CacheKey::UnifyingSlot {
        receiver_uid: receiver_uid.to_string(),
        slot,
    };
    let cached = pass.cache.get(&id);
    let register_kind = map_unifying_kind(event.kind);

    // The 0x41 re-broadcast is the receiver's own slot report and its
    // link-status bit is the liveness authority (Solaar's trigger scan trusts
    // the same bit). The feature/battery refresh below is optional metadata:
    // keep it bounded, never let its one lost reply turn a device that just
    // announced itself into "offline" — and don't probe an offline slot at
    // all, which would burn the budget on a link the receiver just reported
    // as not established.
    let probe_budget = unifying_probe_budget(cached, pass.now, pass.timeouts);
    let probe_result = timeout(
        probe_budget,
        probe_or_reuse(
            channel,
            slot,
            Some(id.clone()),
            cached,
            event.online,
            pass.now,
            pass.subscriptions,
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
pub(in crate::inventory) fn unifying_probe_budget(
    cached: Option<&Cached>,
    now: Instant,
    timeouts: &ProbeTimeouts,
) -> Duration {
    if cached.is_some_and(|entry| !is_stale(entry, now)) {
        timeouts.unifying_cached_slot_probe_timeout
    } else {
        timeouts.unifying_slot_probe_timeout
    }
}

pub(in crate::inventory) fn assemble_unifying_device(
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
        .read_long_sub_register(0xFF, 0xB5, 0x40 + slot - 1, [0x00, 0x00])
        .await
        .ok()?;
    parse_codename_unifying(&response)
}

/// Parse a Unifying name-register response `[sub, len, data..]` into a string.
/// The device-reported `len` is clamped to the bytes actually present so a
/// bogus length can't over-read the fixed long-register buffer.
pub(in crate::inventory) fn parse_codename_unifying(response: &[u8]) -> Option<String> {
    let len = usize::from(*response.get(1)?).min(response.len().saturating_sub(2));
    core::str::from_utf8(response.get(2..2 + len)?)
        .ok()
        .map(str::to_string)
}
