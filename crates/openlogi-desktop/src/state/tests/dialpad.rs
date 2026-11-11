//! The MX Creative Console's per-device settings the Keys view shows: which
//! device is the dialpad.

use super::*;
use crate::services::ipc::Command;
use openlogi_core::config::DeviceIdentity;
use openlogi_core::device_order::PhysicalDeviceKey;

/// A state whose one device is the MX Creative Dialpad, reached directly
/// over Bluetooth.
pub(crate) fn dialpad_state() -> (AppState, tokio::sync::mpsc::UnboundedReceiver<Command>) {
    let inventory = DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Creative Dialpad".to_string(),
            vendor_id: 0x046d,
            product_id: 0xbc00,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Creative Dialpad".to_string()),
            wpid: None,
            kind: DeviceKind::Dialpad,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 3,
                serial_number: Some("DIALPAD-0001".to_string()),
                unit_id: [0x4f, 0x4c, 0x44, 0x04],
                transports: DeviceTransports::default(),
                model_ids: [0xbc00, 0, 0],
                extended_model_id: 0,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Dialpad)),
            settings_revision: 0,
            hidpp10: false,
        }],
    };
    let resolver = AssetResolver::new();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let state = AppState::new(Sources {
        inventories: &[inventory],
        ..Sources::in_memory(Config::ephemeral(), &resolver, commands)
    });
    (state, receiver)
}

#[test]
fn the_dialpad_is_the_dialpad_and_the_keypad_is_not() {
    let (dialpad, _commands) = dialpad_state();
    assert_eq!(dialpad.current_record().unwrap().kind, DeviceKind::Dialpad);
    assert_eq!(
        dialpad.current_dialpad().map(|dialpad| dialpad.name),
        Some("MX Creative Dialpad")
    );
    assert_eq!(dialpad.current_keypad(), None);

    let (keypad, _commands) = super::keypad_pages::keypad_state();
    assert_eq!(keypad.current_dialpad(), None);
}

/// A sleeping dialpad has no live route; the route its saved entry records
/// still names it, so its dial, roller, and keys stay bindable.
#[test]
fn an_offline_dialpad_is_still_the_dialpad() {
    let offline = |route_key: &str| {
        let mut config = Config::ephemeral();
        config.set_device_identity(
            "serial:dialpad-0001",
            DeviceIdentity {
                display_name: "MX Creative Dialpad".to_string(),
                kind: DeviceKind::Dialpad,
                capabilities: Capabilities::presumed_from_kind(DeviceKind::Dialpad),
                light_capabilities: None,
                model_info: None,
                codename: None,
                driver_id: None,
                registry_model_id: None,
            },
        );
        let canonical = PhysicalDeviceKey::parse("serial:dialpad-0001").unwrap();
        assert!(config.adopt_route(&canonical, route_key, None));
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        AppState::new(Sources::in_memory(config, &AssetResolver::new(), commands))
    };

    let state = offline("direct:046d:bc00");
    assert_eq!(state.current_record().unwrap().route, None);
    assert_eq!(
        state.current_dialpad().map(|dialpad| dialpad.name),
        Some("MX Creative Dialpad")
    );
    assert_eq!(offline("direct:046d:c548").current_dialpad(), None);
}
