//! Probing a directly attached device: Bluetooth-direct or wired, addressed at
//! its own index with no receiver in between, and told apart from a receiver's
//! secondary HID interface by what its feature table exposes.

use std::sync::Arc;

use hidpp::channel::HidppChannel;
use openlogi_core::device::{DeviceInventory, PairedDevice, ReceiverInfo};
use tracing::debug;

use super::{NodeProbe, PassContext, ProbeVerdict};
use crate::backend::NodeInfo;
use crate::channel::route::{DIRECT_DEVICE_INDEX, DeviceRoute};
use crate::inventory::cache::{
    CacheKey, CacheOutcome, Cached, ProbeTarget, UnitIdentity, probe_or_reuse, seen,
};
use crate::inventory::features::{KnownTable, Refresh};
use crate::inventory::mappings::{direct_identity_kind, resolve_device_kind};

/// Prefer the device's own HID++ marketing name over the host HID collection
/// label. Windows Bluetooth frequently exposes only a generic `"Mouse"`, while
/// feature `0x0005` carries the real model name (for example MX Master 2S).
pub(in crate::inventory) fn preferred_direct_codename(
    marketing_name: Option<&str>,
    os_name: &str,
) -> String {
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
pub(super) async fn probe_direct(
    channel: Arc<HidppChannel>,
    info: &NodeInfo,
    pass: PassContext<'_>,
) -> NodeProbe {
    // A transport that reported a permanent disconnect will never answer, and
    // a still-fresh cached probe would not ask: serving it would report the
    // dead channel healthy, so the ledger would never retire it. Windows can
    // drop and re-create a Bluetooth HID node at the same path faster than one
    // reconciliation, so node-vanish retirement does not cover this either.
    if !channel.is_connected() {
        debug!(
            vid = format_args!("{:04x}", info.vendor_id),
            pid = format_args!("{:04x}", info.product_id),
            "channel disconnected — failed probe, keeping last-known identity"
        );
        return failed_probe(info);
    }
    let id = CacheKey::Direct(info.id.clone());
    let returning = match pass.cache.get(&id) {
        Some(_) => None,
        None => returning_walk(&channel, pass).await,
    };
    let cached = pass.cache.get(&id).or(returning.as_ref());
    // A direct device's HID node is present (it is the candidate), so probe it
    // as online: reuse the cached probe while fresh, otherwise probe. Whether
    // the device behind it is linked is the probe's answer, not this call's.
    let (probe, outcome) = probe_or_reuse(
        &channel,
        ProbeTarget {
            index: DIRECT_DEVICE_INDEX,
            registers: None,
        },
        Some(id),
        cached,
        true,
        pass.now,
        pass.subscriptions,
    )
    .await;
    // A returning walk is not under this node's key yet: store it there, or
    // the next pass reads the identity again — with the battery the cache hit
    // re-read, when it had one to re-read.
    let outcome = match (returning, outcome) {
        (Some(_), CacheOutcome::Update(key, cached)) | (Some(cached), CacheOutcome::Seen(key)) => {
            CacheOutcome::Returned {
                key,
                cached,
                route: DeviceRoute::Direct {
                    vendor_id: info.vendor_id,
                    product_id: info.product_id,
                },
            }
        }
        (_, outcome) => outcome,
    };
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
    // trustworthy: without it (the device never answered, or it did not answer
    // its refresh ping and the cache stood in) a rejection below would be
    // indistinguishable from a transient glitch, so the node is settled as a
    // failed probe and its last inventory replayed. A refresh that the device
    // answered but did not finish re-reading still counts: its cached walk is
    // complete, and the ping shows the channel works (#2107).
    let capabilities = probe.capabilities();
    let walk_succeeded = capabilities.is_some() && probe.refresh == Refresh::Answered;
    let caps = capabilities.unwrap_or_default();
    // An unlinked answer came from a battery feature too: a headset dongle
    // whose headset is off.
    let is_peripheral =
        probe.battery.is_some() || probe.unlinked || caps.buttons || caps.pointer || caps.lighting;
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
        return failed_probe(info);
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
            // No receiver pairing register here, so the kind comes from `0x0005`
            // with the identity registry as this path's register (the MX Creative
            // Keypad reports no DeviceType). Kind is just identity now; the UI
            // gates on the capabilities below, so a misread kind can't hide the
            // panels (#127).
            kind: resolve_device_kind(
                probe.kind,
                direct_identity_kind(info.vendor_id, info.product_id),
            ),
            // The HID node is present, but for a headset dongle that says
            // nothing about the headset: its battery feature does.
            online: !probe.unlinked,
            battery: probe.battery,
            model_info: probe.model_info,
            capabilities,
            settings_revision: pass.settings_revision(DIRECT_DEVICE_INDEX),
            hidpp10: false,
        }],
    };
    NodeProbe {
        inventory: Some(inventory),
        verdict: ProbeVerdict::Healthy { complete: true },
        outcomes: vec![outcome],
    }
}

/// The walk a device on a node new to the cache made on a node it has left,
/// re-dated to this pass.
///
/// A Bluetooth device returns on a new OS node every time it powers on, so
/// without this each reconnect walked the whole feature table — tens of
/// round trips over the air — before the device was published and its
/// volatile settings re-applied. Its own `0x0003` unit id and model ids,
/// read live in two requests, prove the unit is the one that made the
/// walk. The table's head, two more, proves the table is the one walked: a
/// unit returning over another transport — Bluetooth, then a cable — or
/// after a firmware update lists another. `None` when no walk is known, the
/// device does not answer, or neither matches: the node then gets a full
/// probe.
async fn returning_walk(channel: &Arc<HidppChannel>, pass: PassContext<'_>) -> Option<Cached> {
    if !pass.direct_walks().any() {
        return None;
    }
    let identity = UnitIdentity::read(channel, DIRECT_DEVICE_INDEX).await?;
    let walk = pass.direct_walks().of_unit(identity)?;
    let table = KnownTable {
        head: walk.probe.table_head?,
        // Proven above.
        unit: None,
    };
    if !table.still_listed(channel, DIRECT_DEVICE_INDEX).await {
        debug!(
            ?identity,
            "direct device returned listing another feature table — walking it"
        );
        return None;
    }
    debug!(
        ?identity,
        "direct device returned on a new node — reusing its walk"
    );
    Some(Cached {
        probed_at: pass.now,
        ..walk.clone()
    })
}

/// A probe that could not check the node. The ledger replays its last-good
/// inventory and counts the failure toward retiring the channel; the cache
/// entry is kept so the device's identity survives the outage.
fn failed_probe(info: &NodeInfo) -> NodeProbe {
    NodeProbe {
        inventory: None,
        verdict: ProbeVerdict::Failed,
        outcomes: vec![seen(Some(CacheKey::Direct(info.id.clone())))],
    }
}
