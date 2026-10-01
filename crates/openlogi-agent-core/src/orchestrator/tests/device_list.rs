//! Building the device list and picking the capture target.

use super::*;

fn raw_light_dev(key: &str) -> AgentDevice {
    AgentDevice {
        config_key: key.to_string(),
        model_key: "Litra Glow".to_string(),
        route: Some(DeviceRoute::RawHid {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".to_string(),
        }),
        slot: DIRECT_DEVICE_INDEX,
        serial: Some("glow-1".to_string()),
        unit_id: [0; 4],
        capabilities: None,
        kind: DeviceKind::Light,
        light_capabilities: Some(openlogi_core::device::LightCapabilities {
            power: true,
            ..openlogi_core::device::LightCapabilities::default()
        }),
        online: true,
    }
}

fn direct_inventory_state(
    product_id: u16,
    serial_number: Option<&str>,
    unit_id: [u8; 4],
    online: bool,
) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".to_string(),
            vendor_id: 0x046d,
            product_id,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: serial_number.map(str::to_string),
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [product_id, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

fn bolt_inventory(unit_id: [u8; 4]) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Bolt Receiver".to_string(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("82839805".to_string()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

#[test]
fn build_devices_still_finds_settings_left_under_a_pre_upgrade_receiver_key() {
    // The agent autostarts at login and never adopts a route — only the GUI
    // does, and the GUI is launched by hand. The schema-4 -> 5 migration
    // deliberately does not rename `receiver:` keys, so if this resolved
    // straight to `unit:6be9d300` every receiver-paired device would apply
    // pure defaults from the moment of the upgrade until the user next
    // happened to open the settings window.
    let mut config = Config::default();
    config
        .devices
        .entry("receiver:82839805:slot:1".to_string())
        .or_default()
        .dpi = Some(Dpi::new(3200));

    let devices = build_devices(&config, &[bolt_inventory([0x6b, 0xe9, 0xd3, 0x00])], &[]);
    let device = devices.first().expect("one paired device");
    assert_eq!(device.config_key, "receiver:82839805:slot:1");
    assert_eq!(
        config
            .devices
            .get(device.config_key.as_str())
            .map(|entry| entry.effective_dpi(&stable_id(device).route_key())),
        Some(Some(Dpi::new(3200))),
        "the DPI the agent re-applies on reconnect is still reachable"
    );
}

#[test]
fn build_devices_skips_transient_zero_unit_direct_identity() {
    assert!(build_devices(&Config::default(), &[direct_inventory(None, [0; 4])], &[]).is_empty());

    let devices = build_devices(
        &Config::default(),
        &[direct_inventory(Some("ABC123"), [0; 4])],
        &[],
    );
    assert_eq!(devices.len(), 1);
    // Bare identity, route-independent: the same key the GUI resolves for
    // this device regardless of which route it's reached by.
    assert_eq!(devices[0].config_key, "serial:abc123");
}

#[test]
fn build_devices_keeps_serial_backed_standalone_lights_beside_hidpp_devices() {
    let light_capabilities = openlogi_core::device::LightCapabilities {
        power: true,
        ..openlogi_core::device::LightCapabilities::default()
    };
    let standalone = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc900,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:glow-1".to_string(),
        },
        display_name: "Litra Glow".to_string(),
        manufacturer: Some("Logitech".to_string()),
        serial_number: Some("Glow-1".to_string()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: Some(light_capabilities),
        driver_id: "litra".to_string(),
        registry_model_id: Some("8c900".to_string()),
    };

    let devices = build_devices(
        &Config::default(),
        &[direct_inventory(Some("ABC123"), [0; 4])],
        &[standalone],
    );

    assert_eq!(devices.len(), 2);
    let Some(light) = devices
        .iter()
        .find(|device| device.model_key == "Litra Glow")
    else {
        panic!("standalone light should be retained");
    };
    // Bare identity, route-independent, same as above.
    assert_eq!(light.config_key, "serial:glow-1");
    assert_eq!(light.light_capabilities, Some(light_capabilities));
    assert!(matches!(light.route, Some(DeviceRoute::RawHid { .. })));
}

/// A cabled direct device whose own identity wasn't readable — the shape an
/// offline probe reports, or a device seen for the first time.
fn direct_stable_id() -> DeviceStableId {
    DeviceStableId::Direct {
        vendor_id: 0x046d,
        product_id: 0xc08d,
        identity: DeviceIdentity::Unit([0; 4]),
    }
}

#[test]
fn the_agent_reads_settings_under_the_device_key() {
    // The agent must look under the same key the GUI wrote, or a cabled
    // mouse silently gets no settings applied.
    let mut config = Config::default();
    let mut device = DeviceConfig::default();
    device
        .links
        .insert("direct:046d:c08d".to_string(), LinkConfig::default());
    config.devices.insert("unit:6be9d300".to_string(), device);
    config.set_dpi("unit:6be9d300", Dpi::new(1600));

    let key = config
        .resolve_device_key(&direct_stable_id(), None)
        .expect("resolves through the indexed route");
    assert_eq!(config.devices[key.as_str()].dpi, Some(Dpi::new(1600)));
}

#[test]
fn standalone_selection_never_replaces_the_hidpp_capture_target() {
    let devices = [raw_light_dev("light"), dev("mouse", 1, true)];

    assert_eq!(pick_current(&devices, Some("light")), 1);
    assert_eq!(pick_current(&devices, None), 1);
}

#[test]
fn runtime_selection_falls_back_from_saved_offline_device_to_online_device() {
    let devices = [dev("saved", 1, false), dev("online", 2, true)];

    assert_eq!(pick_current(&devices, Some("saved")), 1);
}

#[test]
fn runtime_selection_keeps_saved_device_when_it_is_online() {
    let devices = [dev("other", 1, true), dev("saved", 2, true)];

    assert_eq!(pick_current(&devices, Some("saved")), 1);
}

#[test]
fn runtime_selection_keeps_saved_device_when_all_devices_are_offline() {
    let devices = [dev("other", 1, false), dev("saved", 2, false)];

    assert_eq!(pick_current(&devices, Some("saved")), 1);
}

#[test]
fn runtime_selection_tracks_online_transition_without_device_set_change() {
    // Both keys are the bare-identity form `resolve_device_key` returns while
    // the device is online (route-independent, per cross-transport identity).
    let saved_key = "unit:01000000";
    let other_key = "unit:02000000";
    let mut config = Config::default();
    config.set_selected_device(Some(saved_key.to_string()));
    let mut orchestrator = orchestrator(config);

    orchestrator.refresh_inventory(
        &[
            direct_inventory_state(0xb023, None, [1, 0, 0, 0], true),
            direct_inventory_state(0xb034, None, [2, 0, 0, 0], false),
        ],
        &[],
        false,
    );
    assert_eq!(orchestrator.current_key(), Some(saved_key));

    orchestrator.refresh_inventory(
        &[
            direct_inventory_state(0xb023, None, [1, 0, 0, 0], false),
            direct_inventory_state(0xb034, None, [2, 0, 0, 0], true),
        ],
        &[],
        false,
    );
    assert_eq!(orchestrator.current_key(), Some(other_key));
}
