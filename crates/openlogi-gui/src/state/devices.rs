//! Device-list construction and selection helpers for [`super::AppState`].

use openlogi_core::device::{BatteryInfo, DeviceInventory, DeviceKind};
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};

use crate::asset::{AssetResolver, ResolvedAsset};

/// One paired device with everything the UI needs to switch to it in O(1):
/// the config key (for bindings/DPI persistence), a display name, the
/// resolved asset (PNG + metadata, or `None` for the synthetic fallback),
/// and the [`DeviceRoute`] HID++ writes / capture target.
///
/// The `kind` / `slot` / `online` / `battery` fields mirror the source
/// [`PairedDevice`](openlogi_core::device::PairedDevice) so the header
/// carousel can render straight from the device list — the list is the single
/// source of truth for "which devices exist", keeping carousel order aligned
/// with [`super::AppState::current_device`].
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub config_key: String,
    pub display_name: String,
    pub asset: Option<ResolvedAsset>,
    pub serial_number: Option<String>,
    pub unit_id: [u8; 4],
    pub route: Option<DeviceRoute>,
    pub kind: DeviceKind,
    pub slot: u8,
    pub online: bool,
    pub battery: Option<BatteryInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DeviceStableId {
    Bolt {
        receiver_uid: String,
        slot: u8,
    },
    Direct {
        vendor_id: u16,
        product_id: u16,
        identity: DeviceIdentity,
    },
    Unknown {
        slot: u8,
        identity: DeviceIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DeviceIdentity {
    Serial(String),
    Unit([u8; 4]),
}

pub(super) fn build_device_list(
    inventories: &[DeviceInventory],
    cache: &AssetResolver,
) -> Vec<DeviceRecord> {
    let mut list = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            let config_key = model.config_key();
            let asset = cache.resolve(model, paired.codename.as_deref());
            let display_name = asset
                .as_ref()
                .map(|a| a.display_name.clone())
                .or_else(|| paired.codename.clone())
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            list.push(DeviceRecord {
                config_key,
                display_name,
                asset,
                serial_number: model.serial_number.clone(),
                unit_id: model.unit_id,
                route: device_route(inv, paired.slot),
                kind: paired.kind,
                slot: paired.slot,
                online: paired.online,
                battery: paired.battery.clone(),
            });
        }
    }
    // HID enumeration order can change as different mice wake, sleep, or are
    // selected. Keep the header carousel stable by ordering by the physical
    // route instead of by whichever HID node happened to be reported first.
    list.sort_by_key(device_order_key);
    list
}

fn device_order_key(record: &DeviceRecord) -> (DeviceStableId, String, String) {
    (
        DeviceStableId::from_record(record),
        record.config_key.clone(),
        record.display_name.clone(),
    )
}

impl DeviceStableId {
    fn from_record(record: &DeviceRecord) -> Self {
        match &record.route {
            Some(DeviceRoute::Bolt { receiver_uid, slot }) => Self::Bolt {
                receiver_uid: receiver_uid.to_ascii_lowercase(),
                slot: *slot,
            },
            Some(DeviceRoute::Direct {
                vendor_id,
                product_id,
            }) => Self::Direct {
                vendor_id: *vendor_id,
                product_id: *product_id,
                identity: DeviceIdentity::from_record(record),
            },
            None => Self::Unknown {
                slot: record.slot,
                identity: DeviceIdentity::from_record(record),
            },
        }
    }
}

impl DeviceIdentity {
    fn from_record(record: &DeviceRecord) -> Self {
        record.serial_number.as_ref().map_or_else(
            || Self::Unit(record.unit_id),
            |serial| Self::Serial(serial.to_ascii_lowercase()),
        )
    }
}

/// Build the [`DeviceRoute`] HID++ writes use to reach a device.
///
/// A Bolt-paired device routes through its receiver UID + slot. A directly
/// attached one (USB cable / Bluetooth) carries no receiver UID and sits at
/// [`DIRECT_DEVICE_INDEX`] — it routes by the HID node's vendor/product id
/// instead. A Bolt device whose receiver UID couldn't be read gets no route
/// (`None`), so hardware writes are skipped rather than mis-routed to the
/// receiver's own pid.
fn device_route(inv: &DeviceInventory, slot: u8) -> Option<DeviceRoute> {
    match &inv.receiver.unique_id {
        Some(receiver_uid) => Some(DeviceRoute::Bolt {
            receiver_uid: receiver_uid.clone(),
            slot,
        }),
        None if slot == DIRECT_DEVICE_INDEX => Some(DeviceRoute::Direct {
            vendor_id: inv.receiver.vendor_id,
            product_id: inv.receiver.product_id,
        }),
        None => None,
    }
}

pub(super) fn pick_initial_device(list: &[DeviceRecord], saved: Option<&str>) -> usize {
    saved
        .and_then(|key| list.iter().position(|r| r.config_key == key))
        .unwrap_or(0)
}
