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
use crate::transport::{enumerate_hidpp_devices, is_logitech_mouse, open_hidpp_channel};

/// How long to wait for device-arrival event bursts before assuming the
/// receiver has finished reporting. MX Master 4 (and other devices that may
/// be asleep) need a generous window to wake and respond to the arrival
/// ping; we err on the side of waiting.
const ARRIVAL_DRAIN: Duration = Duration::from_millis(1500);

/// Maximum number of pairing slots a Bolt receiver supports. We iterate this
/// range to surface paired-but-offline devices that won't fire arrival events.
const MAX_BOLT_SLOTS: u8 = 6;

/// Logitech vendor ID (mirrors `transport::LOGITECH_VID`; local copy avoids
/// a `pub(crate)` re-export just for this module).
const LOGITECH_VID: u16 = 0x046d;
/// MX Master 4 B (Bluetooth-direct) ProductID — distinct from 0xb042 which is
/// the USB cable / Bolt-receiver variant of the same device family.
const MX_MASTER_4_PID: u16 = 0xb048;
const MX_MASTER_4_EXT_MODEL_ID: u8 = 0x02;
/// Product name as reported by macOS for the BLE-direct variant.
const MX_MASTER_4_NAME: &str = "MX Master 4 B";

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

    // Run the hidutil fallback whenever MX Master 4 B is absent — not only when
    // the list is empty. A Bolt receiver attached alongside the BLE-paired
    // MX Master 4 B would otherwise populate `inventories` and suppress this
    // fallback, leaving the BLE device undetected.
    let already_has_mx4b = inventories.iter().any(|inv| {
        inv.receiver.product_id == MX_MASTER_4_PID
            || inv.paired.iter().any(|p| {
                p.model_info
                    .as_ref()
                    .is_some_and(|m| m.model_ids[0] == MX_MASTER_4_PID)
            })
    });
    if !already_has_mx4b {
        // spawn_blocking so the synchronous hidutil shell-out does not stall
        // the async executor.
        let fallback = tokio::task::spawn_blocking(hidutil_direct_mouse_fallback)
            .await
            .unwrap_or_default();
        inventories.extend(fallback);
    }

    Ok(inventories)
}

/// Synthesise a `DeviceInventory` for a Logitech generic-desktop mouse node
/// when `probe_direct` returned `None` (device doesn't speak HID++).
/// Only fires for the MX Master 4 B PID to keep the scope narrow.
fn fallback_direct_mouse(info: &async_hid::DeviceInfo) -> Option<DeviceInventory> {
    if !is_logitech_mouse(info) || info.product_id != MX_MASTER_4_PID {
        return None;
    }
    debug!(
        name = %info.name,
        pid = format_args!("{:04x}", info.product_id),
        "Logitech mouse node has no HID++ collection — using synthetic inventory"
    );
    Some(synthetic_direct_mouse_inventory(
        &info.name,
        info.product_id,
        MX_MASTER_4_EXT_MODEL_ID,
    ))
}

/// Last-resort fallback: shell out to `hidutil list --ndjson` on macOS to check
/// whether the MX Master 4 B is connected when the HID enumerate path returned
/// nothing (e.g. because `async-hid` couldn't open the device node).
///
/// Confirmed working on macOS 13 (Ventura) and later; `--ndjson` was added no
/// later than macOS 10.15. On non-macOS this is a no-op.
#[cfg(target_os = "macos")]
fn hidutil_direct_mouse_fallback() -> Vec<DeviceInventory> {
    use std::process::Command;
    let output = match Command::new("/usr/bin/hidutil")
        .args([
            "list",
            "--ndjson",
            "--matching",
            // VendorID 0x046d = Logitech; ProductID 0xb048 = MX Master 4 B
            r#"{"VendorID":0x046d,"ProductID":0xb048}"#,
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            debug!(error = %e, "hidutil unavailable; skipping MX Master 4 B fallback");
            return Vec::new();
        }
    };
    if !output.status.success() {
        debug!(
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "hidutil exited non-zero (--ndjson requires macOS 10.15+)"
        );
        return Vec::new();
    }
    if !hidutil_lists_mx_master_4b(&output.stdout) {
        return Vec::new();
    }
    debug!("hidutil found MX Master 4 B (BLE-direct) — synthesising inventory");
    vec![synthetic_direct_mouse_inventory(
        MX_MASTER_4_NAME,
        MX_MASTER_4_PID,
        MX_MASTER_4_EXT_MODEL_ID,
    )]
}

#[cfg(not(target_os = "macos"))]
fn hidutil_direct_mouse_fallback() -> Vec<DeviceInventory> {
    Vec::new()
}

#[cfg(target_os = "macos")]
fn hidutil_lists_mx_master_4b(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout).lines().any(|line| {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        value.get("Product").and_then(|v| v.as_str()) == Some(MX_MASTER_4_NAME)
            && value.get("VendorID").and_then(serde_json::Value::as_u64)
                == Some(u64::from(LOGITECH_VID))
            && value.get("ProductID").and_then(serde_json::Value::as_u64)
                == Some(u64::from(MX_MASTER_4_PID))
            && value
                .get("PrimaryUsagePage")
                .and_then(serde_json::Value::as_u64)
                == Some(1)
            && value
                .get("PrimaryUsage")
                .and_then(serde_json::Value::as_u64)
                == Some(2)
    })
}

fn synthetic_direct_mouse_inventory(
    name: &str,
    product_id: u16,
    extended_model_id: u8,
) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: name.to_string(),
            vendor_id: LOGITECH_VID,
            product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some(name.to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 0,
                unit_id: [0; 4],
                transports: DeviceTransports {
                    btle: true,
                    ..DeviceTransports::default()
                },
                model_ids: [product_id, 0, 0],
                extended_model_id,
            }),
        }],
    }
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
        return Ok(probe_direct(Arc::clone(&channel), &info)
            .await
            .or_else(|| fallback_direct_mouse(&info)));
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
            Ok(info) => Some(DeviceModelInfo {
                entity_count: info.entity_count,
                unit_id: info.unit_id,
                transports: DeviceTransports {
                    usb: info.transport.usb,
                    equad: info.transport.e_quad,
                    btle: info.transport.btle,
                    bluetooth: info.transport.bluetooth,
                },
                model_ids: info.model_id,
                extended_model_id: info.extended_model_id,
            }),
            Err(e) => {
                debug!(slot, error = ?e, "DeviceInformation read failed");
                None
            }
        },
        None => None,
    };

    (battery, model_info)
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
