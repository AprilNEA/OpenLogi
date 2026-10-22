//! Transport-independent inputs for the native battery menus and warnings.

use openlogi_core::config::{BatteryPreferences, Config, canonical_device_key};
use openlogi_core::device::{BatteryInfo, DeviceInventory};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};
use openlogi_core::hid::DeviceRoute;

/// A paired device and the user's independently selected presentation options.
#[derive(Clone, Debug)]
pub struct Observation {
    /// Canonical GUI target when online or identified; otherwise a resolved or runtime key.
    pub key: String,
    /// Stable physical identity for warning persistence, absent for anonymous devices.
    pub history_key: Option<String>,
    /// Route and model evidence for an anonymous pairing, never persisted.
    pub session_key: String,
    /// Offline pairings retain session history but never appear in the menu.
    pub online: bool,
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
        for paired in &inventory.paired {
            let route = DeviceRoute::for_slot(inventory, paired.slot);
            let model = paired.model_info.as_ref();
            let serial = model.and_then(|model| model.serial_number.as_deref());
            let unit = model.map_or([0; 4], |model| model.unit_id);
            let identity = DeviceIdentity::from_parts(serial, unit);
            let stable = DeviceStableId::from_parts(route.as_ref(), paired.slot, serial, unit);
            // A receiver slot identifies a route, not the device paired there.
            let history_key = identity.config_key();
            let session_key = format!("{}:{:?}", stable.runtime_key(), paired.wpid);
            let resolved_key = config.resolve_device_key(&stable, Some(&identity));
            let preferences = resolved_key
                .as_ref()
                .map_or_else(BatteryPreferences::default, |key| {
                    config.battery_preferences(key.as_str())
                });
            let name = resolved_key
                .as_ref()
                .and_then(|key| config.devices.get(key.as_str()))
                .and_then(|device| device.custom_name.clone())
                .or_else(|| paired.codename.clone())
                .unwrap_or_else(|| "Logitech".to_owned());
            let navigation_key = if paired.online || history_key.is_some() {
                canonical_device_key(&stable, Some(&identity))
            } else {
                resolved_key
            };
            let key =
                navigation_key.map_or_else(|| stable.runtime_key(), PhysicalDeviceKey::into_string);
            devices.push((
                stable,
                Observation {
                    key,
                    history_key,
                    session_key,
                    online: paired.online,
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
            if reading_priority(&observation) > reading_priority(existing) {
                existing.battery = observation.battery;
            }
            if existing.history_key.is_none() {
                existing.history_key = observation.history_key;
            }
            existing.online |= observation.online;
        } else {
            unique.push(observation);
        }
    }
    unique
}

// A current charging route outranks a receiver that still reports discharge.
// Keep configuration identity and preferences from the canonical first route.
fn reading_priority(observation: &Observation) -> (bool, bool, bool, bool) {
    observation
        .battery
        .as_ref()
        .map_or((observation.online, false, false, false), |battery| {
            let current = battery.freshness != openlogi_core::device::BatteryFreshness::Cached;
            (
                observation.online,
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
                    percentage: Some(18),
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
                settings_revision: 0,
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
    fn duplicate_transports_use_current_reading_and_preserve_offline_pairings() {
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
        let rows = observations(&config, &[receiver]);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].online);
    }

    #[test]
    fn online_route_supplies_identity_missing_from_offline_pairing() {
        let mut config = Config::default();
        let identity = PhysicalDeviceKey::parse("unit:01020304").unwrap();
        config.adopt_route(&identity, "receiver:receiver:slot:1", None);
        let mut receiver = inventory(0xc548, 1, [0; 4]);
        receiver.paired[0].model_info = None;
        receiver.paired[0].online = false;
        let cable = inventory(0xc343, 0xff, [1, 2, 3, 4]);
        let rows = observations(&config, &[receiver, cable]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].online);
        assert_eq!(rows[0].history_key.as_deref(), Some("unit:01020304"));
    }

    #[test]
    fn online_identity_missing_navigation_follows_the_gui_adoption_target() {
        let mut config = Config::default();
        let physical = PhysicalDeviceKey::parse("unit:01020304").unwrap();
        let route_key = "receiver:receiver:slot:1";
        config.adopt_route(&physical, route_key, None);
        let mut receiver = inventory(0xc548, 1, [0; 4]);
        receiver.paired[0].model_info = None;
        let rows = observations(&config, &[receiver]);
        assert_eq!(rows[0].key, route_key);
        assert!(rows[0].online);
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
    fn battery_navigation_survives_legacy_route_adoption_without_changing_preferences() {
        let mut config = Config::default();
        let legacy_key = "receiver:receiver:slot:1";
        let legacy = config.devices.entry(legacy_key.into()).or_default();
        legacy.custom_name = Some("Work keyboard".into());
        legacy.battery = BatteryPreferences {
            show_in_menu: false,
            warn_low: false,
        };
        let rows = observations(&config, &[inventory(0xc548, 1, [1, 2, 3, 4])]);
        assert_eq!(rows[0].key, "unit:01020304");
        assert_eq!(rows[0].name, "Work keyboard");
        assert!(!rows[0].preferences.show_in_menu);
        assert!(!rows[0].preferences.warn_low);
        assert!(config.devices.contains_key(legacy_key));
        let physical = PhysicalDeviceKey::parse("unit:01020304").unwrap();
        assert!(config.adopt_route(&physical, legacy_key, None));
        let adopted = observations(&config, &[inventory(0xc548, 1, [1, 2, 3, 4])]);
        assert_eq!(adopted[0].key, rows[0].key);
        assert_eq!(adopted[0].name, rows[0].name);
        assert_eq!(adopted[0].preferences, rows[0].preferences);
        let replacement = observations(&config, &[inventory(0xc548, 1, [5, 6, 7, 8])]);
        assert_eq!(replacement[0].key, "unit:05060708");
        assert_ne!(replacement[0].key, rows[0].key);
        assert_eq!(replacement[0].preferences, BatteryPreferences::default());
    }

    #[test]
    fn transient_direct_devices_ignore_previously_saved_battery_opt_outs() {
        let mut config = Config::default();
        let runtime_key = "direct:046d:b369:unit:00000000";
        config.set_battery_preferences(
            runtime_key,
            BatteryPreferences {
                show_in_menu: false,
                warn_low: false,
            },
        );
        let rows = observations(&config, &[inventory(0xb369, 0xff, [0; 4])]);
        assert_eq!(rows[0].key, runtime_key);
        assert_eq!(rows[0].preferences, BatteryPreferences::default());
        assert!(!config.battery_preferences(runtime_key).show_in_menu);
    }

    #[test]
    fn receiver_slot_is_not_a_durable_warning_identity() {
        for product in [0xc548, 0xc52b] {
            let mut receiver = inventory(product, 1, [0; 4]);
            for missing_model in [false, true] {
                if missing_model {
                    receiver.paired[0].model_info = None;
                }
                let rows = observations(&Config::default(), &[receiver.clone()]);
                assert_eq!(rows[0].key, "receiver:receiver:slot:1");
                assert_eq!(rows[0].history_key, None);
            }
        }
    }

    #[test]
    fn receiver_session_key_tracks_model_but_not_sleep() {
        let mut receiver = inventory(0xc548, 1, [0; 4]);
        receiver.paired[0].wpid = Some(0xb369);
        let first = observations(&Config::default(), &[receiver.clone()]).remove(0);
        receiver.paired[0].online = false;
        let sleeping = observations(&Config::default(), &[receiver.clone()]).remove(0);
        assert_eq!(first.session_key, sleeping.session_key);
        receiver.paired[0].online = true;
        receiver.paired[0].wpid = Some(0xb034);
        let replacement = observations(&Config::default(), &[receiver]).remove(0);
        assert_eq!(first.key, replacement.key);
        assert_ne!(first.session_key, replacement.session_key);
        assert_eq!(replacement.history_key, None);
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
