//! Transport-independent inputs for the native battery menus and warnings.

use openlogi_core::config::{BatteryPreferences, Config, canonical_device_key};
use openlogi_core::device::{BatteryInfo, DeviceInventory};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};
use openlogi_core::hid::DeviceRoute;

/// An online device and the user's independently selected presentation options.
#[derive(Clone, Debug)]
pub struct Observation {
    /// The configuration key used by device navigation and preferences.
    pub key: String,
    /// Stable physical identity for warning persistence, absent for anonymous devices.
    pub history_key: Option<String>,
    /// User-assigned alias, or the firmware's device name.
    pub name: String,
    /// The latest reading, which may explicitly be a last-good cache replay.
    pub battery: Option<BatteryInfo>,
    /// Independent menu and notification switches.
    pub preferences: BatteryPreferences,
}

pub(super) fn observations(config: &Config, inventories: &[DeviceInventory]) -> Vec<Observation> {
    let mut devices = Vec::new();
    for inventory in inventories {
        for paired in inventory.paired.iter().filter(|device| device.online) {
            let route = DeviceRoute::for_slot(inventory, paired.slot);
            let model = paired.model_info.as_ref();
            let serial = model.and_then(|model| model.serial_number.as_deref());
            let unit = model.map_or([0; 4], |model| model.unit_id);
            let identity = DeviceIdentity::from_parts(serial, unit);
            let stable = DeviceStableId::from_parts(route.as_ref(), paired.slot, serial, unit);
            let history_key =
                canonical_device_key(&stable, Some(&identity)).map(PhysicalDeviceKey::into_string);
            let key = config
                .resolve_device_key(&stable, Some(&identity))
                .map_or_else(|| stable.runtime_key(), PhysicalDeviceKey::into_string);
            let name = config
                .devices
                .get(&key)
                .and_then(|device| device.custom_name.clone())
                .or_else(|| paired.codename.clone())
                .unwrap_or_else(|| "Logitech".to_owned());
            let preferences = config.battery_preferences(&key);
            devices.push((
                stable,
                Observation {
                    key,
                    history_key,
                    name,
                    battery: paired.battery.clone(),
                    preferences,
                },
            ));
        }
    }
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    // A device can be visible through USB and its receiver at the same time.
    // Prefer a current reading, while preserving the shared canonical order.
    let mut unique: Vec<Observation> = Vec::new();
    for (_, observation) in devices {
        if let Some(existing) = unique.iter_mut().find(|device| {
            device.key == observation.key
                || (device.history_key.is_some() && device.history_key == observation.history_key)
        }) {
            if reading_priority(observation.battery.as_ref())
                > reading_priority(existing.battery.as_ref())
            {
                existing.battery = observation.battery;
            }
        } else {
            unique.push(observation);
        }
    }
    unique
}

// A current charging route outranks a receiver that still reports discharge.
// Keep configuration identity and preferences from the canonical first route.
fn reading_priority(reading: Option<&BatteryInfo>) -> (bool, bool, bool) {
    reading.map_or((false, false, false), |battery| {
        let current = battery.freshness != openlogi_core::device::BatteryFreshness::Cached;
        (
            current,
            current
                && (battery.is_charging()
                    || battery.status == openlogi_core::device::BatteryStatus::Full),
            battery.usable_percentage().is_some(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::device::{
        BatteryFreshness, BatteryLevel, BatteryStatus, DeviceKind, DeviceModelInfo,
        DeviceTransports, PairedDevice, ReceiverInfo,
    };

    fn inventory(product_id: u16, slot: u8, unit: [u8; 4]) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logitech".into(),
                vendor_id: 0x046d,
                product_id,
                unique_id: (slot != 0xff).then(|| "receiver".into()),
            },
            paired: vec![PairedDevice {
                slot,
                codename: Some("Keyboard".into()),
                wpid: None,
                kind: DeviceKind::Keyboard,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: 18,
                    level: BatteryLevel::Good,
                    status: BatteryStatus::Discharging,
                    freshness: BatteryFreshness::Current,
                }),
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    unit_id: unit,
                    transports: DeviceTransports::default(),
                    model_ids: [0xb369, 0, 0],
                    extended_model_id: 0,
                    serial_number: None,
                }),
                capabilities: None,
            }],
        }
    }

    #[test]
    fn battery_settings_and_identity_follow_bolt_unifying_bluetooth_and_usb() {
        let mut config = Config::default();
        let settings = config.devices.entry("unit:01020304".into()).or_default();
        settings.custom_name = Some("Work keyboard".into());
        settings.battery = BatteryPreferences {
            show_in_menu: false,
            warn_low: true,
        };
        for (product_id, slot) in [(0xc548, 1), (0xc52b, 1), (0xb369, 0xff), (0xc343, 0xff)] {
            let rows = observations(&config, &[inventory(product_id, slot, [1, 2, 3, 4])]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].key, "unit:01020304");
            assert_eq!(rows[0].history_key.as_deref(), Some("unit:01020304"));
            assert_eq!(rows[0].name, "Work keyboard");
            assert!(!rows[0].preferences.show_in_menu);
            assert!(rows[0].preferences.warn_low);
        }
    }

    #[test]
    fn duplicate_transports_use_current_reading_and_offline_devices_disappear() {
        let config = Config::default();
        let mut receiver = inventory(0xc548, 1, [1, 2, 3, 4]);
        receiver.paired[0].battery.as_mut().unwrap().freshness = BatteryFreshness::Cached;
        let usb = inventory(0xc343, 0xff, [1, 2, 3, 4]);
        let rows = observations(&config, &[receiver.clone(), usb]);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].battery.as_ref().unwrap().usable_percentage(),
            Some(18)
        );
        receiver.paired[0].online = false;
        assert!(observations(&config, &[receiver]).is_empty());
    }

    #[test]
    fn anonymous_navigation_uses_the_same_runtime_key_as_the_gui() {
        let config = Config::default();
        let direct = inventory(0xb369, 0xff, [0; 4]);
        let rows = observations(&config, &[direct]);
        assert_eq!(rows[0].key, "direct:046d:b369:unit:00000000");
        assert_eq!(rows[0].history_key, None);
    }
    #[test]
    fn charging_usb_route_suppresses_receiver_discharge_warning() {
        let receiver = inventory(0xc548, 1, [1, 2, 3, 4]);
        let mut cable = inventory(0xc343, 0xff, [1, 2, 3, 4]);
        cable.paired[0].battery.as_mut().unwrap().status = BatteryStatus::Charging;
        for routes in [vec![receiver.clone(), cable.clone()], vec![cable, receiver]] {
            let rows = observations(&Config::default(), &routes);
            assert_eq!(rows.len(), 1);
            assert!(rows[0].battery.as_ref().unwrap().is_charging());
            assert!(!rows[0].battery.as_ref().unwrap().needs_attention());
        }
    }
}
