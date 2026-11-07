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
        settings_revision: 0,
        codename: None,
        battery: None,
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
            settings_revision: 0,
            hidpp10: false,
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
            settings_revision: 0,
            hidpp10: false,
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
fn a_model_only_device_does_not_take_the_settings_of_the_unit_that_last_held_its_slot() {
    // The agent never adopts a route, so with the GUI closed the `links` index
    // still names the unit. The device answering there says it is not one.
    let mut config = Config::default();
    let previous = config
        .devices
        .entry("unit:6be9d300".to_string())
        .or_default();
    previous.dpi = Some(Dpi::new(3200));
    previous.links.insert(
        "receiver:82839805:slot:1".to_string(),
        LinkConfig::default(),
    );

    let devices = build_devices(&config, &[bolt_inventory([0; 4])], &[]);

    let device = devices.first().expect("one paired device");
    assert_eq!(device.config_key, "receiver:82839805:slot:1");
}

#[test]
fn a_model_identified_direct_device_cycles_the_presets_saved_under_its_route() {
    // DeviceInformation answered with every identity field zero (the G402
    // shape): the device is one instance of its model, and the agent cycles
    // the DPI presets the GUI saved under its route.
    let presets = vec![Dpi::new(400), Dpi::new(800), Dpi::new(1600)];
    let mut config = Config::default();
    config.set_dpi_presets("direct:046d:c07e", presets.clone());
    let mut orch = orchestrator(config);

    orch.refresh_inventory(
        &[direct_inventory_state(0xc07e, None, [0; 4], true)],
        &[],
        false,
    );

    assert_eq!(orch.devices.len(), 1);
    assert_eq!(orch.devices[0].config_key, "direct:046d:c07e");
    let Ok(cycles) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    let cycle = cycles
        .by_key
        .get("direct:046d:c07e")
        .expect("the agent cycles the device's DPI");
    assert_eq!(cycle.presets, presets);
    assert_eq!(
        cycle.target,
        Some(DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc07e,
        })
    );
}

#[test]
fn units_of_a_model_do_not_take_the_settings_saved_under_its_model_key() {
    // `direct:<vid>:<pid>` holds the settings of a device of that model that
    // reports neither a serial nor a unit id. Nothing says it was either of
    // these mice, so each keeps its own, still empty, entry.
    let mut config = Config::default();
    config.set_dpi("direct:046d:c08d", Dpi::new(1600));
    let mut orch = orchestrator(config);

    orch.refresh_inventory(
        &[
            direct_inventory_state(0xc08d, None, [0x6b, 0xe9, 0xd3, 0x00], true),
            direct_inventory_state(0xc08d, None, [0x01, 0x02, 0x03, 0x04], true),
        ],
        &[],
        false,
    );

    let keys: Vec<&str> = orch
        .devices
        .iter()
        .map(|device| device.config_key.as_str())
        .collect();
    assert_eq!(keys, ["unit:01020304", "unit:6be9d300"]);
}

#[test]
fn build_devices_skips_a_device_whose_identity_was_never_read() {
    // No DeviceInformation answer at all: the agent has nothing to key on and
    // waits for the next probe rather than applying defaults.
    let mut inventory = direct_inventory(None, [0; 4]);
    inventory.paired[0].model_info = None;
    assert!(build_devices(&Config::default(), &[inventory], &[]).is_empty());

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
        identity: Some(DeviceIdentity::Model),
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

#[test]
fn a_setting_changed_on_the_device_is_not_replayed_over() {
    use std::collections::BTreeMap;
    // Fn+Esc on the keyboard, or a DPI button, moves the settings revision.
    // The user changed the setting on the device on purpose, so the agent
    // must not write the persisted value back over it.
    let mut inventory = bolt_inventory([0x6b, 0xe9, 0xd3, 0x00]);
    let before = build_devices(&Config::default(), std::slice::from_ref(&inventory), &[]);
    inventory.paired[0].settings_revision += 1;
    let after = build_devices(&Config::default(), &[inventory], &[]);

    let (targets, followup) = plan_reapply(&before, &after, &BTreeMap::new(), None, false);
    assert!(targets.is_empty());
    assert!(followup.is_empty());
}

/// The config that links `keyboard` to `mouse` for host switches.
fn host_switch_link(keyboard: &str, mouse: &str) -> Config {
    let mut config = Config::default();
    config
        .devices
        .entry(keyboard.into())
        .or_default()
        .host_switch_targets = std::iter::once(mouse.into()).collect();
    config
}

/// A mouse that wakes gets its cookie plan on the refresh that sees it
/// online. Here the plan drops a clear held for a pair that is linked again.
/// A plan that read the previous inventory would still see the mouse offline
/// and keep the clear.
#[test]
fn a_waking_mouse_gets_its_cookie_plan_on_the_refresh_that_sees_it() {
    let (keyboard, mouse) = ("unit:01000000", "unit:02000000");
    let byte = openlogi_hid::LeadCookie::for_unit_id([1, 0, 0, 0]).expect("a non-zero byte");
    let mut orchestrator = orchestrator(host_switch_link(keyboard, mouse));
    orchestrator.follower_cookies().requeue(mouse, [byte]);
    let inventory = |mouse_online| {
        [
            direct_inventory_state(0xb023, None, [1, 0, 0, 0], true),
            direct_inventory_state(0xb034, None, [2, 0, 0, 0], mouse_online),
        ]
    };
    orchestrator.refresh_inventory(&inventory(false), &[], false);
    assert_eq!(orchestrator.follower_cookies().held_for(mouse), [byte]);
    orchestrator.refresh_inventory(&inventory(true), &[], false);
    assert!(orchestrator.follower_cookies().held_for(mouse).is_empty());
}

/// A link removed while its keyboard's receiver is unplugged is cleared with
/// the byte that the agent saw while the keyboard was there. The test mouse
/// has no channel, so the planned clear is held again for its next reconnect.
#[test]
fn a_link_removed_after_the_keyboard_left_is_still_cleared() {
    let (keyboard, mouse) = ("unit:01000000", "unit:02000000");
    let byte = openlogi_hid::LeadCookie::for_unit_id([1, 0, 0, 0]).expect("a non-zero byte");
    let mut orchestrator = orchestrator(host_switch_link(keyboard, mouse));
    let mouse_inventory = direct_inventory_state(0xb034, None, [2, 0, 0, 0], true);
    orchestrator.refresh_inventory(
        &[
            direct_inventory_state(0xb023, None, [1, 0, 0, 0], true),
            mouse_inventory.clone(),
        ],
        &[],
        false,
    );
    orchestrator.refresh_inventory(std::slice::from_ref(&mouse_inventory), &[], false);
    orchestrator.reload_config(Config::default());
    assert_eq!(orchestrator.follower_cookies().held_for(mouse), [byte]);
}

#[test]
fn two_devices_with_their_own_units_on_one_route_are_both_managed() {
    // Two cabled mice of one model share the route but not the key.
    let devices = build_devices(
        &Config::default(),
        &[
            direct_inventory_state(0xc08d, None, [1, 1, 1, 1], true),
            direct_inventory_state(0xc08d, None, [2, 2, 2, 2], true),
        ],
        &[],
    );

    let keys: Vec<&str> = devices
        .iter()
        .map(|device| device.config_key.as_str())
        .collect();
    assert_eq!(keys, ["unit:01010101", "unit:02020202"]);
}

#[test]
fn two_model_only_devices_of_one_model_are_both_managed_under_one_key() {
    // Two G402s report neither a serial nor a unit id, and are reached by
    // the same `direct:<vid>:<pid>` route: nothing tells them apart, so they
    // share the one entry their route keys.
    let g402 = direct_inventory_state(0xc07e, None, [0; 4], true);
    let devices = build_devices(&Config::default(), &[g402.clone(), g402], &[]);

    let keys: Vec<&str> = devices
        .iter()
        .map(|device| device.config_key.as_str())
        .collect();
    assert_eq!(keys, ["direct:046d:c07e", "direct:046d:c07e"]);
}
