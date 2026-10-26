//! Probing a directly attached device: Bluetooth-direct or wired, addressed at
//! its own index with no receiver in between, and told apart from a receiver's
//! secondary HID interface by what its feature table exposes.

use std::sync::Arc;

use hidpp::channel::HidppChannel;
use openlogi_core::device::{DeviceInventory, DeviceModelInfo, PairedDevice, ReceiverInfo};
use tracing::debug;

use super::{NodeProbe, PassContext, ProbeVerdict};
use crate::backend::NodeInfo;
use crate::channel::route::DIRECT_DEVICE_INDEX;
use crate::inventory::cache::{CacheKey, CacheOutcome, ProbeTarget, probe_or_reuse, seen};
use crate::inventory::features::Refresh;
use crate::inventory::mappings::{
    direct_identity_kind, normalize_serial_number, resolve_device_kind,
};

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

/// A direct device (such as the K380) may lack a HID++ 0x0003 serial number,
/// leaving `model_info.serial_number` as `None` and yielding an ephemeral
/// unit id across reconnects. Backfill with the host transport's stable serial
/// (e.g. Bluetooth MAC address or USB descriptor serial).
pub(in crate::inventory) fn backfill_direct_serial(
    model_info: &mut Option<DeviceModelInfo>,
    outcome: &mut CacheOutcome,
    os_serial: Option<&str>,
) {
    let Some(direct_serial) = os_serial.and_then(normalize_serial_number) else {
        return;
    };
    if let Some(model) = model_info.as_mut()
        && model.serial_number.is_none()
    {
        model.serial_number = Some(direct_serial.clone());
    }
    match outcome {
        CacheOutcome::Fresh(_, cached) | CacheOutcome::Update(_, cached) => {
            if let Some(model) = &mut cached.probe.model_info
                && model.serial_number.is_none()
            {
                model.serial_number = Some(direct_serial);
            }
        }
        _ => {}
    }
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
    let cached = pass.cache.get(&id);
    // A direct device's HID node is present (it is the candidate), so probe it
    // as online: reuse the cached probe while fresh, otherwise probe. Whether
    // the device behind it is linked is the probe's answer, not this call's.
    let (mut probe, mut outcome) = probe_or_reuse(
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
    backfill_direct_serial(
        &mut probe.model_info,
        &mut outcome,
        info.serial_number.as_deref(),
    );
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use openlogi_core::device::{DeviceModelInfo, DeviceTransports};

    use super::*;
    use crate::backend::NodeId;
    use crate::inventory::cache::Cached;
    use crate::inventory::events::EventFeatureIndices;
    use crate::inventory::features::ProbedFeatures;

    fn sample_model(serial: Option<&str>) -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 2,
            serial_number: serial.map(str::to_string),
            unit_id: [0xfc, 0, 0, 0],
            transports: DeviceTransports::default(),
            model_ids: [0xb342, 0, 0],
            extended_model_id: 0,
        }
    }

    fn sample_cached(model: Option<DeviceModelInfo>) -> Cached {
        Cached {
            probe: ProbedFeatures {
                model_info: model,
                ..ProbedFeatures::default()
            },
            battery: None,
            events: EventFeatureIndices::default(),
            probed_at: Instant::now(),
        }
    }

    #[test]
    fn direct_device_without_hidpp_serial_inherits_os_serial() {
        let mut model = Some(sample_model(None));
        let key = CacheKey::Direct(NodeId::from("test-node".to_string()));
        let mut outcome = CacheOutcome::Fresh(key, sample_cached(model.clone()));

        backfill_direct_serial(&mut model, &mut outcome, Some("f4:73:35:43:c8:12"));

        assert_eq!(
            model.as_ref().and_then(|m| m.serial_number.as_deref()),
            Some("f4:73:35:43:c8:12")
        );
        match outcome {
            CacheOutcome::Fresh(_, cached) => {
                assert_eq!(
                    cached
                        .probe
                        .model_info
                        .as_ref()
                        .and_then(|m| m.serial_number.as_deref()),
                    Some("f4:73:35:43:c8:12")
                );
            }
            _ => panic!("expected Fresh outcome"),
        }
    }

    #[test]
    fn direct_device_in_update_outcome_inherits_os_serial() {
        let mut model = Some(sample_model(None));
        let key = CacheKey::Direct(NodeId::from("test-node".to_string()));
        let mut outcome = CacheOutcome::Update(key, sample_cached(model.clone()));

        backfill_direct_serial(&mut model, &mut outcome, Some("f4:73:35:43:c8:12"));

        assert_eq!(
            model.as_ref().and_then(|m| m.serial_number.as_deref()),
            Some("f4:73:35:43:c8:12")
        );
        match outcome {
            CacheOutcome::Update(_, cached) => {
                assert_eq!(
                    cached
                        .probe
                        .model_info
                        .as_ref()
                        .and_then(|m| m.serial_number.as_deref()),
                    Some("f4:73:35:43:c8:12")
                );
            }
            _ => panic!("expected Update outcome"),
        }
    }

    #[test]
    fn direct_device_with_existing_hidpp_serial_is_not_overwritten() {
        let mut model = Some(sample_model(Some("1945LZ04A8")));
        let key = CacheKey::Direct(NodeId::from("test-node".to_string()));
        let mut outcome = CacheOutcome::Fresh(key, sample_cached(model.clone()));

        backfill_direct_serial(&mut model, &mut outcome, Some("f4:73:35:43:c8:12"));

        assert_eq!(
            model.as_ref().and_then(|m| m.serial_number.as_deref()),
            Some("1945LZ04A8")
        );
        match outcome {
            CacheOutcome::Fresh(_, cached) => {
                assert_eq!(
                    cached
                        .probe
                        .model_info
                        .as_ref()
                        .and_then(|m| m.serial_number.as_deref()),
                    Some("1945LZ04A8")
                );
            }
            _ => panic!("expected Fresh outcome"),
        }
    }

    #[test]
    fn direct_device_with_empty_or_whitespace_os_serial_remains_none() {
        let mut model = Some(sample_model(None));
        let key = CacheKey::Direct(NodeId::from("test-node".to_string()));
        let mut outcome = CacheOutcome::Fresh(key, sample_cached(model.clone()));

        backfill_direct_serial(&mut model, &mut outcome, Some("  \0\0 "));

        assert!(
            model
                .as_ref()
                .and_then(|m| m.serial_number.as_deref())
                .is_none()
        );
    }
}
