//! Probing a directly attached device: Bluetooth-direct or wired, addressed at
//! its own index with no receiver in between, and told apart from a receiver's
//! secondary HID interface by what its feature table exposes.

use std::sync::Arc;

use hidpp::channel::HidppChannel;
use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
use tracing::debug;

use super::{NodeProbe, PassContext, ProbeVerdict};
use crate::backend::NodeInfo;
use crate::channel::route::DIRECT_DEVICE_INDEX;
use crate::inventory::cache::{CacheKey, CacheOutcome, probe_or_reuse, seen};
use crate::inventory::mappings::resolve_device_kind;

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
    let id = CacheKey::Direct(info.id.clone());
    let cached = pass.cache.get(&id);
    // A direct device is always "present" (its HID node is the candidate), so
    // treat it as online: reuse the cached probe while fresh, otherwise probe.
    let (probe, outcome) = probe_or_reuse(
        &channel,
        DIRECT_DEVICE_INDEX,
        Some(id),
        cached,
        true,
        pass.now,
        pass.subscriptions,
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
