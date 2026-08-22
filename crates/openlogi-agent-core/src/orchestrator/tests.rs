//! Orchestrator inventory/reapply/camera tests.

use super::{
    AgentDevice, InventoryHealth, Orchestrator, VOLATILE_REAPPLY_CONFIRM_RETRIES,
    any_device_needs_capture_rearm, build_devices, configured_wheel_mode, host_switch_links,
    pick_current, plan_reapply, reapply_targets, stable_id,
};
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{Action, Binding, ButtonId};
use openlogi_core::config::{
    Config, DeviceConfig, LightSettings, LinkConfig, ScrollResolution, VerticalScrollSensitivity,
};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
    LightCapabilities, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::Dpi;
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};
use std::sync::Arc;

use crate::observable::ObservableState;

/// An orchestrator wired to a state cell nobody subscribes to. The publishing
/// paths still run, so a mutator that stops republishing shows up here rather
/// than only in the running agent.
fn orchestrator(config: Config) -> Orchestrator {
    Orchestrator::new(config, Arc::new(ObservableState::new("test".to_string())))
}

fn dev(key: &str, slot: u8, online: bool) -> AgentDevice {
    AgentDevice {
        config_key: key.to_string(),
        model_key: key.to_string(),
        route: Some(DeviceRoute::Bolt {
            receiver_uid: "AA00".to_string(),
            slot,
        }),
        slot,
        serial: None,
        unit_id: [0; 4],
        capabilities: None,
        kind: openlogi_core::device::DeviceKind::Mouse,
        light_capabilities: None,
        online,
    }
}

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

fn direct_inventory(serial_number: Option<&str>, unit_id: [u8; 4]) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".to_string(),
            vendor_id: 0x046d,
            product_id: 0xb023,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".to_string()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: serial_number.map(str::to_string),
                unit_id,
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
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

#[test]
fn configured_wheel_mode_gates_resolution_and_inversion_independently() {
    let mut config = Config::default();
    config.set_scroll_resolution("a", Some(ScrollResolution::Low));
    config.set_invert_scroll("a", true);
    let mut device = dev("a", 1, true);

    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        scroll_inversion: false,
        ..Capabilities::default()
    });
    assert_eq!(
        configured_wheel_mode(&config, &device),
        (Some(ScrollResolution::Low), None)
    );

    device.capabilities = Some(Capabilities {
        hires_wheel: false,
        scroll_inversion: true,
        ..Capabilities::default()
    });
    assert_eq!(configured_wheel_mode(&config, &device), (None, Some(true)));

    device.capabilities = None;
    assert_eq!(configured_wheel_mode(&config, &device), (None, None));
}

#[test]
fn configured_wheel_mode_leaves_unset_resolution_unmanaged() {
    let config = Config::default();
    let mut device = dev("a", 1, true);
    device.capabilities = Some(Capabilities {
        hires_wheel: true,
        scroll_inversion: false,
        ..Capabilities::default()
    });

    assert_eq!(configured_wheel_mode(&config, &device), (None, None));
}

#[test]
fn host_switch_links_keep_sleeping_targets_but_require_online_keyboard() {
    let mut config = Config::default();
    config
        .devices
        .entry("keyboard".into())
        .or_default()
        .host_switch_targets = vec!["mouse".into(), "offline".into(), "missing".into()];
    let devices = [
        dev("keyboard", 1, true),
        dev("mouse", 2, true),
        dev("offline", 3, false),
    ];

    let links = host_switch_links(&config, &devices);

    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0].keyboard,
        DeviceRoute::Bolt {
            receiver_uid: "AA00".into(),
            slot: 1,
        }
    );
    assert_eq!(
        links[0].targets,
        vec![
            DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 2,
            },
            DeviceRoute::Bolt {
                receiver_uid: "AA00".into(),
                slot: 3,
            }
        ]
    );
}

#[test]
fn reapply_targets_new_arrivals_and_transitions() {
    // First sighting of an online device → re-apply.
    assert_eq!(reapply_targets(&[], &[dev("a", 1, true)], false), vec![0]);
    // Steady state → nothing.
    assert!(reapply_targets(&[dev("a", 1, true)], &[dev("a", 1, true)], false).is_empty());
    // Replug under a new route (same key, new slot) → re-apply.
    assert_eq!(
        reapply_targets(&[dev("a", 1, true)], &[dev("a", 2, true)], false),
        vec![0]
    );
    // Waking from device sleep (offline → online) → re-apply.
    assert_eq!(
        reapply_targets(&[dev("a", 1, false)], &[dev("a", 1, true)], false),
        vec![0]
    );
    // Going to sleep (online → offline) → nothing.
    assert!(reapply_targets(&[dev("a", 1, true)], &[dev("a", 1, false)], false).is_empty());
}

#[test]
fn dpi_cycle_drops_offline_device_and_restores_on_return() {
    let mut orch = orchestrator(Config::default());
    orch.devices = vec![dev("mouse", 1, true)];
    orch.rebuild();
    {
        let Ok(mut dpi) = orch.shared.dpi_cycle.write() else {
            panic!("DPI cycle lock should not be poisoned");
        };
        if let Some(state) = dpi.by_key.get_mut("mouse") {
            state.index = 3;
        }
    }

    orch.devices[0].online = false;
    orch.publish_device_runtime();
    {
        let Ok(dpi) = orch.shared.dpi_cycle.read() else {
            panic!("DPI cycle lock should not be poisoned");
        };
        assert!(!dpi.by_key.contains_key("mouse"));
    }

    orch.devices[0].online = true;
    orch.publish_device_runtime();
    let Ok(dpi) = orch.shared.dpi_cycle.read() else {
        panic!("DPI cycle lock should not be poisoned");
    };
    assert_eq!(dpi.by_key.get("mouse").map(|s| s.index), Some(0));
    assert_eq!(
        dpi.by_key.get("mouse").and_then(|s| s.target.clone()),
        orch.devices[0].route
    );
}

#[test]
fn reapply_targets_disambiguates_same_model_duplicates() {
    // Two devices can share a model key but are distinct physical units at
    // different Bolt slots, so they have distinct stable ids. A steady tick
    // with both already online must target NEITHER.
    let prev = [dev("dup", 1, true), dev("dup", 2, true)];
    let next = [dev("dup", 1, true), dev("dup", 2, true)];
    assert!(reapply_targets(&prev, &next, false).is_empty());
}

#[test]
fn reapply_targets_skip_offline_and_routeless_devices() {
    // A paired-but-asleep new arrival waits for its online transition —
    // writing now would only time out against a sleeping device.
    assert!(reapply_targets(&[], &[dev("a", 1, false)], false).is_empty());
    let routeless = AgentDevice {
        route: None,
        ..dev("b", 2, true)
    };
    assert!(reapply_targets(&[], &[routeless], false).is_empty());
}

#[test]
fn reapply_all_targets_every_online_device() {
    let prev = [dev("a", 1, true), dev("b", 2, false)];
    let next = [dev("a", 1, true), dev("b", 2, false)];
    // The post-wake snapshot looks identical to the pre-sleep one; the
    // flag still re-applies to the online device (and only that one).
    assert_eq!(reapply_targets(&prev, &next, true), vec![0]);
}

#[test]
fn receiver_reconnect_requests_capture_rearm() {
    let prev = [dev("selected", 1, false), dev("other", 2, true)];
    let next = [dev("selected", 1, true), dev("other", 2, true)];

    assert!(any_device_needs_capture_rearm(&prev, &next, false));
    assert!(!any_device_needs_capture_rearm(
        &[dev("other", 2, true)],
        &[dev("other", 2, true)],
        false
    ));
}

#[test]
fn system_wake_requests_capture_rearm_for_online_devices() {
    let devices = [dev("selected", 1, true), dev("other", 2, true)];

    assert!(any_device_needs_capture_rearm(&devices, &devices, true));
}

#[test]
fn steady_inventory_does_not_cycle_capture() {
    let devices = [dev("selected", 1, true)];

    assert!(!any_device_needs_capture_rearm(&devices, &devices, false));
}

#[test]
fn plan_reapply_retries_a_first_sighting_for_a_bounded_run() {
    use std::collections::HashMap;
    // First sighting: applied now, queued for VOLATILE_REAPPLY_CONFIRM_RETRIES
    // confirming re-applies. A cold restart can leave the device still
    // booting, so the initial write and a single confirm need a retry run,
    // not a one-shot confirm.
    let (targets, followup) = plan_reapply(&[], &[dev("a", 1, true)], &HashMap::new(), false);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)])
    );
    // Each steady tick after a first sighting re-applies once and decrements
    // the remaining retry budget — the device may still be booting.
    let prev = [dev("a", 1, true)];
    let followup_in = HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)]);
    let (targets, followup) = plan_reapply(&prev, &prev, &followup_in, false);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES - 1)])
    );
    // The budget exhausts: a last retry fires but queues no further ones.
    let followup_in = HashMap::from([("a".to_string(), 1)]);
    let (targets, followup) = plan_reapply(&prev, &prev, &followup_in, false);
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
    // Steady state after that: nothing.
    let (targets, _) = plan_reapply(&prev, &prev, &HashMap::new(), false);
    assert!(targets.is_empty());
}

#[test]
fn plan_reapply_transitions_are_not_queued_for_confirmation() {
    use std::collections::HashMap;
    // A wake from device sleep re-applies once — the device was already
    // booted, so no confirming write is queued.
    let (targets, followup) = plan_reapply(
        &[dev("a", 1, false)],
        &[dev("a", 1, true)],
        &HashMap::new(),
        false,
    );
    assert_eq!(targets, vec![0]);
    assert!(followup.is_empty());
}

#[test]
fn plan_reapply_wake_targets_get_a_confirm_retry_run() {
    use std::collections::HashMap;
    // A system wake re-applies to every online device *and* queues the same
    // confirm-retry run a first sighting gets: post-wake, a receiver can
    // enumerate while its mouse link is still re-establishing, so the first
    // write can time out just like the cold-boot race (#527). Offline devices
    // stay untargeted and unqueued; they re-apply on their own transition.
    let prev = [dev("a", 1, true), dev("b", 2, false)];
    let (targets, followup) = plan_reapply(&prev, &prev, &HashMap::new(), true);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)])
    );
    // The run then drains at the usual cadence on steady ticks.
    let (targets, followup) = plan_reapply(&prev, &prev, &followup, false);
    assert_eq!(targets, vec![0]);
    assert_eq!(
        followup,
        HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES - 1)])
    );
}

#[test]
fn plan_reapply_skips_a_followup_that_went_offline() {
    use std::collections::HashMap;
    let prev = [dev("a", 1, true)];
    let (targets, followup) = plan_reapply(
        &prev,
        &[dev("a", 1, false)],
        &HashMap::from([("a".to_string(), VOLATILE_REAPPLY_CONFIRM_RETRIES)]),
        false,
    );
    assert!(targets.is_empty());
    assert!(followup.is_empty());
}

/// An *empty* snapshot still flips the health to `Ready`: the watcher only
/// forwards completed enumerations, so "checked and found nothing" must not
/// be reported as "still scanning" — that's the whole distinction the
/// health exists to carry.
#[test]
fn empty_refresh_marks_inventory_ready() {
    let mut orch = orchestrator(Config::default());
    assert_eq!(orch.inventory_health(), InventoryHealth::Scanning);
    orch.refresh_inventory(&[], &[], false);
    assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
}

/// `Unavailable` is a startup-only downgrade: it reports "enumeration has
/// never worked", recovers when a snapshot finally lands, and never
/// clobbers a live device set on a mid-session failure (mirroring the
/// watcher's keep-last-snapshot policy).
#[test]
fn unavailable_only_downgrades_a_pending_inventory() {
    let mut orch = orchestrator(Config::default());
    orch.mark_inventory_unavailable();
    assert_eq!(orch.inventory_health(), InventoryHealth::Unavailable);
    orch.refresh_inventory(&[], &[], false);
    assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
    orch.mark_inventory_unavailable();
    assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
}

#[test]
fn every_inventory_mutator_republishes_what_the_ipc_server_answers() {
    let observable = Arc::new(ObservableState::new("test".to_string()));
    let mut orch = Orchestrator::new(Config::default(), Arc::clone(&observable));
    assert_eq!(
        observable.snapshot().status.inventory,
        InventoryHealth::Scanning,
        "a fresh agent has not enumerated yet"
    );

    orch.mark_inventory_unavailable();
    assert_eq!(
        observable.snapshot().status.inventory,
        InventoryHealth::Unavailable
    );

    orch.refresh_inventory(
        &[direct_inventory(Some("serial-1"), [1, 2, 3, 4])],
        &[],
        false,
    );
    let published = observable.snapshot();
    assert_eq!(published.status.inventory, InventoryHealth::Ready);
    assert_eq!(published.inventory, orch.inventory());
    assert_eq!(published.standalone, orch.standalone());
    assert_eq!(published.inventory.len(), 1);

    // A camera sample and a config reload are the other two facts the cell
    // carries; both must reach it from inside the mutator.
    orch.set_camera_active(true);
    assert!(observable.snapshot().camera_active);

    let mut config = Config::default();
    config.app_settings.launch_at_login = true;
    orch.reload_config(config);
    assert!(observable.snapshot().status.launch_at_login);
}

#[test]
fn camera_automation_overrides_only_effective_power() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config);

    orch.set_camera_active(false);
    assert_eq!(
        orch.effective_light_settings(key).map(|light| (
            light.enabled,
            light.brightness_percent,
            light.temperature_kelvin
        )),
        Some((false, 65, Some(4600)))
    );

    orch.set_camera_active(true);
    assert_eq!(
        orch.effective_light_settings(key).map(|light| (
            light.enabled,
            light.brightness_percent,
            light.temperature_kelvin
        )),
        Some((true, 65, Some(4600)))
    );
}

#[test]
fn manual_camera_light_override_is_transient() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let route = DeviceRoute::Bolt {
        receiver_uid: "AA00".to_string(),
        slot: 1,
    };
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config);
    orch.set_camera_active(true);
    let mut device = dev(key, 1, true);
    device.light_capabilities = Some(LightCapabilities {
        power: true,
        ..LightCapabilities::default()
    });
    orch.devices.push(AgentDevice {
        config_key: key.to_string(),
        route: Some(route.clone()),
        ..device
    });

    assert!(orch.set_manual_light_power(&route, false));
    assert_eq!(
        orch.effective_light_settings(key)
            .map(|light| light.enabled),
        Some(false)
    );

    orch.devices.clear();
    orch.set_camera_active(false);
    orch.set_camera_active(true);
    assert_eq!(
        orch.effective_light_settings(key)
            .map(|light| light.enabled),
        Some(true)
    );
}

#[test]
fn config_reload_keeps_manual_override_for_parameter_edits() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: false,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config.clone());
    orch.set_camera_active(false);
    orch.manual_light_overrides.insert(key.to_string(), true);

    let mut updated = config;
    updated.set_light(
        key,
        LightSettings {
            enabled: false,
            auto_camera: true,
            brightness_percent: 80,
            temperature_kelvin: Some(6500),
            color: None,
        },
    );
    orch.reload_config(updated);

    assert_eq!(
        orch.effective_light_settings(key).map(|light| (
            light.enabled,
            light.brightness_percent,
            light.temperature_kelvin
        )),
        Some((true, 80, Some(6500)))
    );
    assert_eq!(orch.manual_light_overrides.get(key), Some(&true));
}

#[test]
fn config_reload_publishes_scroll_preferences_without_restarting_the_hook() {
    let mut orch = orchestrator(Config::default());
    let preferences = Arc::clone(&orch.shared.scroll_preferences);
    assert!(!preferences.smooth_scroll_enabled());
    assert_eq!(
        preferences.vertical_sensitivity(),
        VerticalScrollSensitivity::DEFAULT
    );

    let mut config = Config::default();
    config.app_settings.smooth_scroll = true;
    config.app_settings.vertical_scroll_sensitivity =
        VerticalScrollSensitivity::try_new(7).expect("valid sensitivity");
    orch.reload_config(config);

    assert!(preferences.smooth_scroll_enabled());
    assert_eq!(
        preferences.vertical_sensitivity(),
        VerticalScrollSensitivity::try_new(7).expect("valid sensitivity")
    );
}

#[test]
fn config_reload_clears_override_when_camera_mode_changes() {
    let key = "raw:046d:c900:ff43:0202:serial:glow";
    let mut config = Config::default();
    config.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let mut orch = orchestrator(config.clone());
    orch.manual_light_overrides.insert(key.to_string(), false);

    let mut updated = config;
    updated.set_light(
        key,
        LightSettings {
            enabled: true,
            auto_camera: false,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    orch.reload_config(updated);

    assert_eq!(orch.manual_light_overrides.get(key), None);
    assert_eq!(
        orch.effective_light_settings(key)
            .map(|light| light.enabled),
        Some(true)
    );
}

/// The published capture plan's Back binding for the first device, if any.
fn published_back_binding(orch: &Orchestrator) -> Option<Action> {
    orch.shared.capture_plans.read().ok().and_then(|plans| {
        plans.first().and_then(|plan| {
            plan.bindings
                .get(&ButtonId::Back)
                .map(Binding::click_action)
        })
    })
}

#[test]
fn app_switch_republishes_capture_plans() {
    // HID++ dispatch reads `plan.bindings` at event time, so a
    // foreground-app change must republish the capture plans — their
    // binding maps and divert sets are per-app effective — or every
    // diverted button keeps firing the previous app's actions.
    let mut config = Config::default();
    config.set_per_app_binding(
        "a",
        "com.example.editor",
        ButtonId::Back,
        Some(Action::Undo),
    );
    let mut orch = orchestrator(config);
    orch.devices = vec![dev("a", 1, true)];
    orch.rebuild();
    assert_ne!(
        published_back_binding(&orch),
        Some(Action::Undo),
        "no per-app overlay while no app is in front"
    );
    orch.set_current_app(Some(ForegroundApp::unnamed("com.example.editor".into())));
    assert_eq!(published_back_binding(&orch), Some(Action::Undo));
}

/// A directly-attached mouse with inversion configured, either reporting native
/// HID++ inversion or not.
fn direct_dev(key: &str, product_id: u16, native_inversion: bool) -> AgentDevice {
    AgentDevice {
        route: Some(DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id,
        }),
        capabilities: Some(Capabilities {
            scroll_inversion: native_inversion,
            ..Capabilities::default()
        }),
        ..dev(key, DIRECT_DEVICE_INDEX, true)
    }
}

#[test]
fn software_scroll_inversion_skips_native_and_receiver_paired_devices() {
    use std::collections::BTreeSet;

    let mut config = Config::default();
    for key in ["native", "software", "receiver"] {
        config.set_invert_scroll(key, true);
    }
    let mut orch = orchestrator(config);
    orch.devices = vec![
        // Native inversion goes to the firmware; rewriting on top would cancel.
        direct_dev("native", 0xb034, true),
        direct_dev("software", 0xb020, false),
        // A receiver route reports ids shared by every device on that dongle.
        dev("receiver", 1, true),
    ];

    assert_eq!(
        orch.software_scroll_inversion(),
        BTreeSet::from([(0x046d_u32, 0xb020_u32)])
    );
}

#[test]
fn identical_devices_disagreeing_on_inversion_invert_neither() {
    use std::collections::BTreeSet;

    // The hook sees only vendor/product, so it cannot honour one setting without
    // also applying it to the twin. Inverting a wheel nobody asked for is worse.
    let mut config = Config::default();
    config.set_invert_scroll("left-hand", true);
    let mut orch = orchestrator(config);
    orch.devices = vec![
        direct_dev("left-hand", 0xb020, false),
        direct_dev("right-hand", 0xb020, false),
    ];

    assert_eq!(orch.software_scroll_inversion(), BTreeSet::new());
}

#[test]
fn a_disabled_twin_blocks_its_siblings_software_inversion() {
    use std::collections::BTreeSet;

    // The hook cannot rewrite one twin without the other, so a device the user
    // switched off must not be dragged along by its sibling's setting.
    let mut config = Config::default();
    config.set_invert_scroll("enabled", true);
    config.set_device_enabled("switched-off", false);
    let mut orch = orchestrator(config);
    orch.devices = vec![
        direct_dev("enabled", 0xb020, false),
        direct_dev("switched-off", 0xb020, false),
    ];

    assert_eq!(orch.software_scroll_inversion(), BTreeSet::new());
}

#[test]
fn an_offline_twin_holds_no_vote_on_inversion() {
    use std::collections::BTreeSet;

    // It emits no scroll events, so the rewrite cannot reach it — vetoing on its
    // behalf would strand the mouse actually on the desk.
    let mut config = Config::default();
    config.set_invert_scroll("on-the-desk", true);
    let mut orch = orchestrator(config);
    let mut in_a_drawer = direct_dev("in-a-drawer", 0xb020, false);
    in_a_drawer.online = false;
    orch.devices = vec![direct_dev("on-the-desk", 0xb020, false), in_a_drawer];

    assert_eq!(
        orch.software_scroll_inversion(),
        BTreeSet::from([(0x046d_u32, 0xb020_u32)])
    );
}

#[test]
fn a_natively_capable_twin_blocks_software_inversion() {
    use std::collections::BTreeSet;

    // Its setting is already in firmware, and the hook would rewrite it too —
    // inverting that wheel twice.
    let mut config = Config::default();
    config.set_invert_scroll("software", true);
    config.set_invert_scroll("native", true);
    let mut orch = orchestrator(config);
    orch.devices = vec![
        direct_dev("software", 0xb020, false),
        direct_dev("native", 0xb020, true),
    ];

    assert_eq!(orch.software_scroll_inversion(), BTreeSet::new());
}

#[test]
fn identical_devices_agreeing_on_inversion_yield_one_entry() {
    use std::collections::BTreeSet;

    let mut config = Config::default();
    config.set_invert_scroll("left-hand", true);
    config.set_invert_scroll("right-hand", true);
    let mut orch = orchestrator(config);
    orch.devices = vec![
        direct_dev("left-hand", 0xb020, false),
        direct_dev("right-hand", 0xb020, false),
    ];

    assert_eq!(
        orch.software_scroll_inversion(),
        BTreeSet::from([(0x046d_u32, 0xb020_u32)])
    );
}

#[test]
fn disabling_the_selected_device_keeps_another_devices_software_inversion() {
    use std::collections::BTreeSet;

    // Inversion is keyed per device, so muting one cannot stop inverting another.
    let mut config = Config::default();
    config.set_invert_scroll("other", true);
    config.set_device_enabled("selected", false);
    let mut orch = orchestrator(config);
    orch.devices = vec![
        direct_dev("selected", 0xb034, false),
        direct_dev("other", 0xb020, false),
    ];

    let maps = orch.hook_maps_for(Some("selected"), None);
    assert!(
        maps.bindings.is_empty(),
        "a disabled selection remaps nothing"
    );
    assert_eq!(
        maps.invert_scroll,
        BTreeSet::from([(0x046d_u32, 0xb020_u32)])
    );
}
