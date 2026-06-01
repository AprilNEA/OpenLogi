//! Enumerate connected HID++ receivers and their paired devices.

use std::{collections::HashMap, sync::Arc, time::Duration};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        device_information::v0::DeviceInformationFeatureV0,
        unified_battery::v0::{
            BatteryLevel as HidppBatteryLevel, BatteryStatus as HidppBatteryStatus,
            UnifiedBatteryFeatureV0,
        },
    },
    nibble::U4,
    receiver::{
        self, Receiver,
        bolt::{BoltDeviceConnection, BoltDeviceKind, BoltEvent, BoltReceiver},
    },
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, DeviceModelInfo,
    DeviceTransports, PairedDevice, ReceiverInfo,
};
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::route::DIRECT_DEVICE_INDEX;
use crate::transport::{enumerate_hidpp_devices, open_hidpp_channel};

/// How long to wait for device-arrival event bursts before assuming the
/// receiver has finished reporting. MX Master 4 (and other devices that may
/// be asleep) need a generous window to wake and respond to the arrival
/// ping; we err on the side of waiting.
const ARRIVAL_DRAIN: Duration = Duration::from_millis(1500);

/// Maximum number of pairing slots a Bolt receiver supports. We iterate this
/// range to surface paired-but-offline devices that won't fire arrival events.
const MAX_BOLT_SLOTS: u8 = 6;

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
}

/// Enumerate all Logitech HID++ receivers visible to the current process and
/// the devices paired to each.
///
/// Combines two data sources per receiver:
///
/// - `trigger_device_arrival` events — the only path to a device's wireless
///   PID in hidpp 0.2 (the `wpid` field on `BoltDevicePairingInformation` is
///   private). Only online, responsive devices show up here.
/// - `get_device_pairing_information` polled per slot — covers paired-but-
///   offline devices (sleeping mice, devices on a different host) that the
///   arrival ping doesn't wake. No wpid for these.
///
/// We merge the two so an MX Master that's been asleep still shows up with
/// its codename and kind even before you click it.
pub async fn enumerate() -> Result<Vec<DeviceInventory>, InventoryError> {
    let candidates = enumerate_hidpp_devices().await?;

    debug!(count = candidates.len(), "HID++ candidate interfaces");

    let mut inventories = Vec::new();
    for dev in candidates {
        match probe_one(dev).await {
            Ok(Some(inv)) => inventories.push(inv),
            Ok(None) => {}
            Err(e) => warn!(error = ?e, "skipping device that failed to probe"),
        }
    }

    Ok(inventories)
}

async fn probe_one(dev: async_hid::Device) -> Result<Option<DeviceInventory>, InventoryError> {
    let Some((info, channel)) = open_hidpp_channel(dev).await? else {
        return Ok(None);
    };

    let Some(Receiver::Bolt(bolt)) = receiver::detect(Arc::clone(&channel)) else {
        // No receiver detected — this might be a directly-paired device
        // (Bluetooth-direct, USB-C cable). HID++ at device-index 0xff
        // addresses the device's own features. Probe in case it answers.
        // P2.4 — verified path; no Bolt-pairing slot indirection needed.
        return Ok(probe_direct(channel, &info).await);
    };

    let unique_id = bolt.get_unique_id().await.ok();
    let pairing_count = bolt.count_pairings().await.ok();
    debug!(?pairing_count, "receiver reports pairing count");

    let connections = drain_device_arrival(&bolt).await;
    debug!(events = connections.len(), "drained device-arrival events");
    let by_slot: HashMap<u8, BoltDeviceConnection> =
        connections.into_iter().map(|c| (c.index, c)).collect();

    let mut paired = Vec::new();
    for slot in 1u8..=MAX_BOLT_SLOTS {
        let pairing = match bolt.get_device_pairing_information(U4::from_lo(slot)).await {
            Ok(p) => p,
            Err(e) => {
                debug!(slot, error = ?e, "slot empty or unreadable");
                continue;
            }
        };

        let codename = read_codename(&channel, slot).await;
        let event = by_slot.get(&slot);
        // Prefer event data when present — it's a live response. Fall back to
        // the pairing register for sleeping devices that didn't reply.
        let online = event.map_or(pairing.online, |c| c.online);
        let kind = event.map_or(pairing.kind, |c| c.kind);
        let wpid = event.map(|c| c.wpid);
        debug!(
            slot,
            online,
            ?wpid,
            ?kind,
            has_event = event.is_some(),
            codename = ?codename,
            "paired slot"
        );

        let (battery, model_info) = if online {
            probe_features(&channel, slot).await
        } else {
            (None, None)
        };
        paired.push(PairedDevice {
            slot,
            codename,
            wpid,
            kind: map_kind(kind),
            online,
            battery,
            model_info,
        });
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

    Ok(Some(DeviceInventory {
        receiver: ReceiverInfo {
            name: "Logi Bolt Receiver".to_string(),
            vendor_id: info.vendor_id,
            product_id: info.product_id,
            unique_id,
        },
        paired,
    }))
}

/// Probe a HID++ channel that doesn't host a Bolt receiver — for
/// Bluetooth-direct, USB-C, or otherwise wired devices that present
/// themselves as a HID++ device rather than a receiver (P2.4).
///
/// Addresses the device at index `0xff` (HID++'s "self" slot) and reads
/// the same battery + model-info features the Bolt path uses. Returns
/// `None` when the channel doesn't respond to HID++ at `0xff` (in which
/// case it's neither a receiver nor a direct device we recognise).
async fn probe_direct(
    channel: Arc<HidppChannel>,
    info: &async_hid::DeviceInfo,
) -> Option<DeviceInventory> {
    let (battery, model_info) = probe_features(&channel, DIRECT_DEVICE_INDEX).await;
    // Hybrid peripheral discriminator. A genuine directly-attached device is
    // either wireless/Bluetooth — which reports a battery — or wired, which
    // reports none but still exposes a control feature (adjustable DPI or
    // reprogrammable buttons). A Bolt receiver's secondary HID interface also
    // answers DeviceInformation at 0xff, but exposes neither battery nor those
    // control features, so it's filtered out here. Without this guard a Bolt
    // setup ends up with two entries in `device_list`: the real mouse (via the
    // Bolt path) and a phantom "direct device" pointing at the receiver, which
    // sits at index 0 and steals every DPI / SmartShift write attempt.
    //
    // Battery is the fast path (no extra round-trips); the feature probe only
    // runs for battery-less devices, so wired mice cost one more lookup while
    // the common wireless case is unaffected.
    let is_peripheral =
        battery.is_some() || exposes_peripheral_feature(&channel, DIRECT_DEVICE_INDEX).await;
    if !is_peripheral {
        debug!(
            vid = format_args!("{:04x}", info.vendor_id),
            pid = format_args!("{:04x}", info.product_id),
            has_model = model_info.is_some(),
            "slot 0xff exposes no battery or control feature — likely a receiver \
             secondary interface; skipping"
        );
        return None;
    }

    // Without a Bolt receiver we don't have a wpid, codename, or pairing
    // info — those live on the receiver registers. Use the HID name as
    // the display fallback and leave wpid empty.
    debug!(name = %info.name, "BT-direct / wired device recognised");
    Some(DeviceInventory {
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
            kind: DeviceKind::Unknown,
            online: true,
            battery,
            model_info,
        }],
    })
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

/// Open a HID++ session for `slot` and query the two features we care about
/// (battery, device-information) in one shot. Returns `(battery, model)` —
/// either side may be `None` if the device doesn't expose that feature or
/// the read fails. Device sessions are expensive (multi-round-trip) so we
/// fold both reads through the same `Device::new` + `enumerate_features`.
async fn probe_features(
    channel: &Arc<HidppChannel>,
    slot: u8,
) -> (Option<BatteryInfo>, Option<DeviceModelInfo>) {
    let mut device = match Device::new(Arc::clone(channel), slot).await {
        Ok(d) => d,
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            return (None, None);
        }
    };
    if let Err(e) = device.enumerate_features().await {
        debug!(slot, error = ?e, "enumerate_features failed");
        return (None, None);
    }

    let battery = match device.get_feature::<UnifiedBatteryFeatureV0>() {
        Some(feature) => feature
            .get_battery_info()
            .await
            .ok()
            .map(|info| BatteryInfo {
                percentage: info.charging_percentage,
                level: map_battery_level(info.level),
                status: map_battery_status(info.status),
            }),
        None => None,
    };

    let model_info = match device.get_feature::<DeviceInformationFeatureV0>() {
        Some(feature) => match feature.get_device_info().await {
            Ok(info) => {
                let serial_number = if info.capabilities.serial_number {
                    match feature.get_serial_number().await {
                        Ok(serial) => normalize_serial_number(&serial),
                        Err(e) => {
                            debug!(slot, error = ?e, "DeviceInformation serial read failed");
                            None
                        }
                    }
                } else {
                    None
                };
                Some(DeviceModelInfo {
                    entity_count: info.entity_count,
                    serial_number,
                    unit_id: info.unit_id,
                    transports: DeviceTransports {
                        usb: info.transport.usb,
                        equad: info.transport.e_quad,
                        btle: info.transport.btle,
                        bluetooth: info.transport.bluetooth,
                    },
                    model_ids: info.model_id,
                    extended_model_id: info.extended_model_id,
                })
            }
            Err(e) => {
                debug!(slot, error = ?e, "DeviceInformation read failed");
                None
            }
        },
        None => None,
    };

    (battery, model_info)
}

fn normalize_serial_number(serial: &str) -> Option<String> {
    let serial = serial.trim_matches('\0').trim().to_string();
    (!serial.is_empty()).then_some(serial)
}

/// HID++ feature IDs that mark a device as a controllable peripheral rather
/// than a bare receiver interface: adjustable DPI (both encodings) and
/// reprogrammable controls. Used by [`probe_direct`]'s hybrid discriminator
/// to admit wired mice, which report no battery.
const PERIPHERAL_FEATURE_IDS: [u16; 3] = [
    0x2201, // AdjustableDpi
    0x2202, // ExtendedAdjustableDpi
    0x1b04, // ReprogControlsV4
];

/// Whether the device at `index` announces any [`PERIPHERAL_FEATURE_IDS`].
/// Looks each up through the device root — hidpp 0.2's feature registry
/// doesn't carry these, so `enumerate_features` wouldn't surface them (see
/// `write::open_feature`).
async fn exposes_peripheral_feature(channel: &Arc<HidppChannel>, index: u8) -> bool {
    let device = match Device::new(Arc::clone(channel), index).await {
        Ok(d) => d,
        Err(e) => {
            debug!(index, error = ?e, "Device::new failed during peripheral probe");
            return false;
        }
    };
    for id in PERIPHERAL_FEATURE_IDS {
        match device.root().get_feature(id).await {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(e) => debug!(index, id, error = ?e, "root feature probe failed"),
        }
    }
    false
}

fn map_kind(k: BoltDeviceKind) -> DeviceKind {
    match k {
        BoltDeviceKind::Keyboard => DeviceKind::Keyboard,
        BoltDeviceKind::Mouse => DeviceKind::Mouse,
        BoltDeviceKind::Numpad => DeviceKind::Numpad,
        BoltDeviceKind::Presenter => DeviceKind::Presenter,
        BoltDeviceKind::Remote => DeviceKind::Remote,
        BoltDeviceKind::Trackball => DeviceKind::Trackball,
        BoltDeviceKind::Touchpad => DeviceKind::Touchpad,
        BoltDeviceKind::Tablet => DeviceKind::Tablet,
        BoltDeviceKind::Gamepad => DeviceKind::Gamepad,
        BoltDeviceKind::Joystick => DeviceKind::Joystick,
        BoltDeviceKind::Headset => DeviceKind::Headset,
        _ => DeviceKind::Unknown,
    }
}

fn map_battery_level(level: HidppBatteryLevel) -> BatteryLevel {
    match level {
        HidppBatteryLevel::Critical => BatteryLevel::Critical,
        HidppBatteryLevel::Low => BatteryLevel::Low,
        HidppBatteryLevel::Good => BatteryLevel::Good,
        HidppBatteryLevel::Full => BatteryLevel::Full,
        _ => BatteryLevel::Unknown,
    }
}

fn map_battery_status(status: HidppBatteryStatus) -> BatteryStatus {
    match status {
        HidppBatteryStatus::Discharging => BatteryStatus::Discharging,
        HidppBatteryStatus::Charging => BatteryStatus::Charging,
        HidppBatteryStatus::ChargingSlow => BatteryStatus::ChargingSlow,
        HidppBatteryStatus::Full => BatteryStatus::Full,
        HidppBatteryStatus::Error => BatteryStatus::Error,
        _ => BatteryStatus::Unknown,
    }
}
