use openlogi_core::config::{Config, DeviceConfig, LinkConfig};
use openlogi_core::device::{
    DeviceInventory, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
};

use crate::services::assets::AssetResolver;

use std::collections::HashSet;

use super::{
    Camera, Capabilities, DeviceIdentity, DeviceKind, DeviceModelInfo, DeviceRecord,
    DeviceTransports, append_offline_known, build_device_list, direct_key_prefix, effective_kind,
    fold_by_inventory_key, offline_record, pick_initial_device,
};
use crate::state::inventory::adopt_routes;
use openlogi_core::hid::Dpi;

fn paired_device_no_model_info(slot: u8, wpid: Option<u16>) -> PairedDevice {
    PairedDevice {
        slot,
        codename: None,
        wpid,
        kind: DeviceKind::Keyboard,
        online: true,
        battery: None,
        model_info: None,
        capabilities: None,
    }
}

fn inventory_with(devices: Vec<PairedDevice>) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Unifying Receiver".into(),
            vendor_id: 0x046d,
            product_id: 0xc52b,
            unique_id: Some("DA2699E1".into()),
        },
        paired: devices,
    }
}

fn direct_inventory(model_info: DeviceModelInfo) -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".into(),
            vendor_id: 0x046d,
            product_id: 0xb023,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(model_info),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

/// The same mouse, paired to a Bolt receiver — reachable by receiver UID
/// and slot. Shares `unit_id` and `online: true` with [`cabled_inventory`]
/// so both routes resolve to the same physical device.
fn receiver_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "Bolt Receiver".into(),
            vendor_id: 0x046d,
            product_id: 0xc548,
            unique_id: Some("82839805".into()),
        },
        paired: vec![PairedDevice {
            slot: 1,
            codename: Some("MX Master 3S".into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id: [0x6b, 0xe9, 0xd3, 0x00],
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

/// The same mouse, attached directly over a cable — reachable by its own
/// vendor/product id. Shares `unit_id` and `online: true` with
/// [`receiver_inventory`] so both routes resolve to the same physical
/// device.
fn cabled_inventory() -> DeviceInventory {
    DeviceInventory {
        receiver: ReceiverInfo {
            name: "MX Master 3S".into(),
            vendor_id: 0x046d,
            product_id: 0xc08d,
            unique_id: None,
        },
        paired: vec![PairedDevice {
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            codename: Some("MX Master 3S".into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: None,
                unit_id: [0x6b, 0xe9, 0xd3, 0x00],
                transports: DeviceTransports::default(),
                model_ids: [0xc08d, 0, 0],
                extended_model_id: 2,
            }),
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        }],
    }
}

/// Build the device list and fold it the way
/// [`super::super::AppState::merge_inventory_snapshot`] does on every
/// refresh. This calls the real [`fold_by_inventory_key`] that method
/// uses, so the two cannot drift; the rest of that method (transient
/// adoption, miss grace, prior-selection carry-over) is deliberately not
/// reproduced. Folding alone is enough to prove [`build_device_list`]
/// resolves one config key for a device sighted on two routes in the same
/// snapshot.
fn records_from(config: &Config, inventories: &[DeviceInventory]) -> Vec<DeviceRecord> {
    let cache = AssetResolver::new();
    let list = build_device_list(inventories, &[], &cache, config, &[]);
    fold_by_inventory_key(list).into_values().collect()
}

#[test]
fn one_mouse_on_two_routes_is_one_record() {
    // The user-visible symptom: the same mouse listed twice, once offline
    // on its receiver and once live on the cable.
    let config = Config::default();
    let records = records_from(&config, &[receiver_inventory(), cabled_inventory()]);
    assert_eq!(records.len(), 1, "got {records:#?}");
    assert_eq!(records[0].config_key, "unit:6be9d300");
}

fn online_record(key: &str) -> DeviceRecord {
    DeviceRecord {
        config_key: key.to_string(),
        canonical_key: None,
        persistent: true,
        route_key: key.to_string(),
        model_key: key.to_string(),
        model_name: format!("live {key}"),
        display_name: format!("live {key}"),
        asset: None,
        model_info: None,
        codename: None,
        serial_number: None,
        unit_id: [1; 4],
        driver_id: None,
        registry_model_id: None,
        route: None,
        capture_id: None,
        kind: DeviceKind::Mouse,
        capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
        light_capabilities: None,
        slot: 1,
        online: true,
        battery: None,
    }
}

#[test]
fn folding_two_records_of_one_device_keeps_the_online_one() {
    // The surviving record carries the route every HID++ write goes to.
    // Picking the sleeping one writes into a dead link while the device is
    // in active use — and the UI shows it offline while the user is using
    // it. Insertion order must not decide this, so both are tried.
    for live_first in [true, false] {
        let live = online_record("unit:6be9d300");
        let mut asleep = online_record("unit:6be9d300");
        asleep.online = false;
        asleep.route_key = "direct:046d:c08d".to_string();
        let list = if live_first {
            vec![live, asleep]
        } else {
            vec![asleep, live]
        };

        let folded = fold_by_inventory_key(list);
        let record = &folded["unit:6be9d300"];
        assert!(record.online, "live_first = {live_first}");
        assert_eq!(
            record.route_key, "unit:6be9d300",
            "the live record's route survives, live_first = {live_first}"
        );
    }
}

fn receiver_only_config() -> Config {
    let mut config = Config::default();
    config
        .devices
        .entry("receiver:82839805:slot:1".to_string())
        .or_default()
        .dpi = Some(Dpi::new(3200));
    config
}

#[test]
fn a_receiver_paired_device_still_reads_its_pre_upgrade_entry() {
    // Straight after the schema-5 upgrade the settings are under the
    // receiver key the migration deliberately left alone, and only the
    // GUI ever folds them. Until it does, that key is the answer.
    let config = receiver_only_config();
    let records = records_from(&config, &[receiver_inventory()]);
    assert_eq!(records.len(), 1, "got {records:#?}");
    assert_eq!(records[0].config_key, "receiver:82839805:slot:1");
    assert_eq!(
        records[0].canonical_key.as_deref(),
        Some("unit:6be9d300"),
        "the fold target is known even while the settings are elsewhere"
    );
}

#[test]
fn adoption_folds_the_pre_upgrade_entry_and_converges() {
    // Adoption keys off the record's canonical key, not its current
    // `config_key`. Folding onto `config_key` would fold the legacy entry
    // onto itself and nothing would ever move.
    let mut config = receiver_only_config();
    let cache = AssetResolver::new();
    let list = build_device_list(&[receiver_inventory()], &[], &cache, &config, &[]);
    assert!(adopt_routes(&mut config, &list), "the fold is a change");

    assert_eq!(
        config.devices["unit:6be9d300"].dpi,
        Some(Dpi::new(3200)),
        "the DPI moved to the identity key"
    );
    assert!(
        !config.devices.contains_key("receiver:82839805:slot:1"),
        "the legacy entry is consumed"
    );
    let list = build_device_list(&[receiver_inventory()], &[], &cache, &config, &[]);
    assert_eq!(
        list[0].config_key, "unit:6be9d300",
        "the next build reads the canonical key"
    );
}

fn mouse_identity(name: &str) -> DeviceIdentity {
    DeviceIdentity {
        display_name: name.to_string(),
        kind: DeviceKind::Mouse,
        capabilities: Capabilities {
            buttons: true,
            pointer: true,
            lighting: false,
            scroll_inversion: false,
            hires_wheel: false,
            thumbwheel: false,
            haptic_feedback: false,
            haptic_panel: false,
            gaming_buttons: false,
        },
        light_capabilities: None,
        model_info: None,
        codename: None,
        driver_id: None,
        registry_model_id: None,
    }
}

#[test]
fn standalone_registry_identity_is_preserved_without_hidpp_model_info() {
    let device = StandaloneDevice {
        address: RawDeviceAddress {
            vendor_id: 0x046d,
            product_id: 0xc901,
            usage_page: 0xff43,
            usage_id: 0x0202,
            identity: "serial:beam-1".into(),
        },
        display_name: "Future Litra model".into(),
        manufacturer: Some("Logi".into()),
        serial_number: Some("beam-1".into()),
        unit_id: [0; 4],
        kind: DeviceKind::Light,
        online: true,
        capabilities: None,
        light_capabilities: None,
        driver_id: "litra".into(),
        registry_model_id: Some("8c901".into()),
    };
    let list = build_device_list(
        &[],
        std::slice::from_ref(&device),
        &AssetResolver::new(),
        &Config::default(),
        &[],
    );

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].driver_id.as_deref(), Some("litra"));
    assert_eq!(list[0].registry_model_id.as_deref(), Some("8c901"));
    assert_eq!(list[0].model_key, "raw:c901");
    // Online with a known serial: the device's own identity resolves the
    // key, transport-free — not the route-embedded `raw:…` runtime key.
    assert_eq!(list[0].config_key, "serial:beam-1");
    assert!(list[0].asset.is_none());
}

#[test]
fn no_model_info_uses_receiver_slot_as_config_key() {
    let inv = inventory_with(vec![paired_device_no_model_info(1, Some(0x4076))]);
    let cache = AssetResolver::new();
    let list = build_device_list(&[inv], &[], &cache, &Config::default(), &[]);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].config_key, "receiver:da2699e1:slot:1");
    assert_eq!(list[0].model_key, "wpid4076");
    assert!(list[0].serial_number.is_none());
    assert_eq!(list[0].unit_id, [0u8; 4]);
}

#[test]
fn no_model_info_falls_back_to_slot_when_no_wpid() {
    let inv = inventory_with(vec![paired_device_no_model_info(3, None)]);
    let cache = AssetResolver::new();
    let list = build_device_list(&[inv], &[], &cache, &Config::default(), &[]);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].config_key, "receiver:da2699e1:slot:3");
    assert_eq!(list[0].model_key, "slot3");
}

#[test]
fn no_model_info_display_name_falls_back_to_slot() {
    let inv = inventory_with(vec![paired_device_no_model_info(2, Some(0x4051))]);
    let cache = AssetResolver::new();
    let list = build_device_list(&[inv], &[], &cache, &Config::default(), &[]);
    assert_eq!(list[0].display_name, "Slot 2");
}

#[test]
fn saved_custom_name_identifies_the_device_without_replacing_its_model_name() {
    let inv = inventory_with(vec![paired_device_no_model_info(2, Some(0x4051))]);
    let mut config = Config::default();
    config.set_device_custom_name("receiver:da2699e1:slot:2", Some("Office keyboard".into()));

    let list = build_device_list(&[inv], &[], &AssetResolver::new(), &config, &[]);

    assert_eq!(list[0].display_name, "Office keyboard");
    assert_eq!(list[0].model_name, "Slot 2");
}

#[test]
fn offline_record_is_present_but_inert() {
    // A persisted identity renders as an offline card that still carries its
    // measured capabilities (so its panels show) but no route (so writes are
    // no-ops until it wakes).
    let id = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();
    let rec = offline_record("2b034", &id, &cache);
    assert_eq!(rec.config_key, "2b034");
    assert_eq!(rec.display_name, "MX Master 3S");
    assert!(!rec.online);
    assert!(rec.route.is_none());
    assert_eq!(rec.capabilities, Some(id.capabilities));
}

#[test]
fn offline_standalone_record_keeps_registry_and_physical_keys() {
    let id = DeviceIdentity {
        display_name: "Litra Glow".into(),
        kind: DeviceKind::Light,
        capabilities: Capabilities::default(),
        light_capabilities: None,
        model_info: None,
        codename: None,
        driver_id: Some("litra".into()),
        registry_model_id: Some("8c900".into()),
    };
    let record = offline_record(
        "raw:046d:c900:ff43:0202:serial:known-light",
        &id,
        &AssetResolver::new(),
    );

    assert_eq!(record.registry_model_id.as_deref(), Some("8c900"));
    assert_eq!(
        record.model_key,
        "raw:046d:c900:ff43:0202:serial:known-light"
    );
    assert_eq!(
        record.config_key,
        "raw:046d:c900:ff43:0202:serial:known-light"
    );
    assert!(record.model_info.is_none());
}

#[test]
fn known_devices_are_appended_only_when_absent_from_live() {
    // "A" is live; "B" is known-but-asleep. The union keeps the live "A"
    // untouched and adds "B" back as an offline placeholder — the core of
    // the #159 fix: a sleeping device never drops out of the list.
    let mut list = vec![online_record("A")];
    let a = mouse_identity("live A overwritten?");
    let b = mouse_identity("asleep B");
    let cache = AssetResolver::new();
    append_offline_known(
        &mut list,
        [("A", &a), ("B", &b)].into_iter(),
        &cache,
        &HashSet::new(),
        &Config::default(),
    );

    assert_eq!(list.len(), 2);
    assert!(
        list.iter().any(|r| r.config_key == "A" && r.online),
        "the live record for A must win over its identity"
    );
    assert!(
        list.iter().any(|r| r.config_key == "B" && !r.online),
        "B is added back as a persisted offline placeholder"
    );
}

fn model_info(ext: u8, pid: u16) -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports::default(),
        model_ids: [pid, 0, 0],
        extended_model_id: ext,
    }
}

#[test]
fn zero_unit_direct_inventory_is_transient() {
    let cache = AssetResolver::new();
    let list = build_device_list(
        &[direct_inventory(model_info(2, 0xb034))],
        &[],
        &cache,
        &Config::default(),
        &[],
    );

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].config_key, "direct:046d:b023:unit:00000000");
    assert!(!list[0].is_persistent());
    assert!(list[0].persistent_config_key().is_none());
}

#[test]
fn historical_zero_unit_identity_does_not_create_offline_card() {
    let id = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();
    let mut list = Vec::new();

    append_offline_known(
        &mut list,
        [("direct:046d:b023:unit:00000000", &id)].into_iter(),
        &cache,
        &HashSet::new(),
        &Config::default(),
    );

    assert!(list.is_empty());
}

#[test]
fn same_model_physical_bluetooth_devices_remain_distinct() {
    let mut id_a = mouse_identity("MX Master 3S");
    id_a.model_info = Some(model_info(2, 0xb034));
    let id_b = id_a.clone();
    let cache = AssetResolver::new();
    let mut list = Vec::new();

    append_offline_known(
        &mut list,
        [
            ("direct:046d:b023:unit:01020304", &id_a),
            ("direct:046d:b023:unit:05060708", &id_b),
        ]
        .into_iter(),
        &cache,
        &HashSet::new(),
        &Config::default(),
    );

    assert_eq!(list.len(), 2);
}

#[test]
fn persisted_selection_does_not_target_transient_identity() {
    let stable = online_record("receiver:aabb:slot:1");
    let mut transient = online_record("direct:046d:b023:unit:00000000");
    transient.persistent = false;
    let list = vec![stable, transient];

    assert_eq!(
        pick_initial_device(&list, Some("direct:046d:b023:unit:00000000")),
        0
    );
}

#[test]
fn placeholders_for_absent_receivers_are_hidden() {
    // The work receiver's mouse must not haunt the list at home: with its
    // receiver unplugged the device is unreachable, so no card is shown.
    let id = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();
    let mut list = Vec::new();
    append_offline_known(
        &mut list,
        [("receiver:aabb:slot:1", &id)].into_iter(),
        &cache,
        &HashSet::new(),
        &Config::default(),
    );
    assert!(list.is_empty());
    append_offline_known(
        &mut list,
        [("receiver:aabb:slot:1", &id)].into_iter(),
        &cache,
        &HashSet::from(["aabb".to_string()]),
        &Config::default(),
    );
    assert_eq!(list.len(), 1);
}

#[test]
fn adopted_placeholder_is_hidden_when_its_linked_receiver_is_absent() {
    // Once a Bolt device is adopted its entry key becomes its own
    // identity (`unit:…`), not `receiver:<uid>:slot:<n>` — reachability
    // must be resolved through the entry's `links`, not its key, or the
    // work receiver's mouse haunts the list at home again.
    let mut config = Config::default();
    let mut device = DeviceConfig::default();
    device
        .links
        .insert("receiver:aabb:slot:1".to_string(), LinkConfig::default());
    config.devices.insert("unit:6be9d300".to_string(), device);
    let id = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();

    let mut list = Vec::new();
    append_offline_known(
        &mut list,
        [("unit:6be9d300", &id)].into_iter(),
        &cache,
        &HashSet::new(),
        &config,
    );
    assert!(list.is_empty());

    append_offline_known(
        &mut list,
        [("unit:6be9d300", &id)].into_iter(),
        &cache,
        &HashSet::from(["aabb".to_string()]),
        &config,
    );
    assert_eq!(list.len(), 1);
}

#[test]
fn adopted_placeholder_stays_visible_with_a_non_receiver_link_too() {
    // A device seen both over a receiver and directly by cable is not
    // unreachable just because its receiver link's receiver is absent —
    // the recorded direct route might still reach it. Hiding the card
    // would dent the very "a sleeping device never drops out of the
    // list" invariant this function exists to uphold.
    let mut config = Config::default();
    let mut device = DeviceConfig::default();
    device
        .links
        .insert("receiver:aabb:slot:1".to_string(), LinkConfig::default());
    device
        .links
        .insert("direct:046d:c08d".to_string(), LinkConfig::default());
    config.devices.insert("unit:cafebabe".to_string(), device);
    let id = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();

    let mut list = Vec::new();
    append_offline_known(
        &mut list,
        [("unit:cafebabe", &id)].into_iter(),
        &cache,
        &HashSet::new(),
        &config,
    );
    assert_eq!(
        list.len(),
        1,
        "a recorded direct link keeps the entry reachable even though its receiver link's receiver is absent"
    );
}

#[test]
fn same_model_placeholder_is_blocked_by_a_live_unit() {
    // #271: the live mouse reads ext-model 02 while the stale identity was
    // recorded as 00 — the wire PID still identifies them as one model, so
    // the phantom card is suppressed.
    let mut live = online_record("receiver:aabb:slot:2");
    live.model_key = "2b034".to_string();
    live.model_info = Some(model_info(2, 0xb034));
    let mut list = vec![live];
    let id = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();
    append_offline_known(
        &mut list,
        [("0b034", &id)].into_iter(),
        &cache,
        &HashSet::new(),
        &Config::default(),
    );
    assert_eq!(list.len(), 1);
}

#[test]
fn legacy_same_model_placeholders_collapse_to_one_card() {
    // Two persisted identities of one model render identically — a second
    // offline card carries no information, only confusion.
    let id_a = mouse_identity("MX Master 3S");
    let id_b = mouse_identity("MX Master 3S");
    let cache = AssetResolver::new();
    let mut list = Vec::new();
    append_offline_known(
        &mut list,
        [("0b034", &id_a), ("2b034", &id_b)].into_iter(),
        &cache,
        &HashSet::new(),
        &Config::default(),
    );
    assert_eq!(list.len(), 1);
}

#[test]
fn direct_key_prefix_names_the_wire_product() {
    assert_eq!(
        direct_key_prefix("direct:046d:c09d:unit:46002e00"),
        Some("direct:046d:c09d")
    );
    assert_eq!(
        direct_key_prefix("direct:046d:c09d:serial:abc123"),
        Some("direct:046d:c09d")
    );
    assert_eq!(
        direct_key_prefix("direct:046d:c09d:unit:00000000"),
        Some("direct:046d:c09d"),
        "transient keys share the prefix of their physical siblings"
    );
}

#[test]
fn non_direct_keys_have_no_wire_prefix() {
    assert_eq!(direct_key_prefix("receiver:da2699e1:slot:1"), None);
    assert_eq!(direct_key_prefix("unknown:slot:0:unit:00000000"), None);
    assert_eq!(direct_key_prefix("2b034"), None);
    assert_eq!(direct_key_prefix("direct:046d:c09d:"), None);
    assert_eq!(direct_key_prefix("direct:046d"), None);
}

#[test]
fn asset_kind_overrides_a_misreporting_hid_kind() {
    // #127: the registry knows this depot is a mouse, so a HID++ source that
    // reported `Keyboard` loses.
    assert_eq!(
        effective_kind(DeviceKind::Keyboard, Some(DeviceKind::Mouse)),
        DeviceKind::Mouse
    );
}

#[test]
fn hid_kind_is_used_without_a_modelled_asset() {
    // No asset, or an asset whose type we don't model → keep the HID kind.
    assert_eq!(effective_kind(DeviceKind::Mouse, None), DeviceKind::Mouse);
}

#[test]
fn webcams_are_appended_as_camera_records() {
    // A discovered UVC webcam joins the list as a routeless Camera record.
    // With a USB serial the config key is port-stable; capture_id keeps the
    // OS open id the preview needs.
    let camera = Camera {
        name: "Logitech StreamCam".to_string(),
        unique_id: "0x1123000046d0893".to_string(),
        serial_number: Some("ABC123".to_string()),
        vendor_id: 0x046d,
        product_id: 0x0893,
        max_resolution: Some((1920, 1080)),
        max_fps: Some(60),
    };
    let cache = AssetResolver::new();
    let list = build_device_list(&[], &[], &cache, &Config::default(), &[camera]);

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].kind, DeviceKind::Camera);
    assert_eq!(list[0].config_key, "camera:046d:0893:serial:abc123");
    assert_eq!(list[0].record_key(), list[0].config_key);
    assert_eq!(list[0].capture_id.as_deref(), Some("0x1123000046d0893"));
    assert_eq!(list[0].serial_number.as_deref(), Some("ABC123"));
    assert_eq!(list[0].display_name, "Logitech StreamCam");
    assert!(list[0].route.is_none());
    assert!(list[0].capabilities.is_none());
    assert!(list[0].online);
}

#[test]
fn webcam_without_serial_uses_model_scoped_key() {
    let camera = Camera {
        name: "Logitech C920".to_string(),
        unique_id: "0x14110000046d082d".to_string(),
        serial_number: None,
        vendor_id: 0x046d,
        product_id: 0x082d,
        max_resolution: None,
        max_fps: None,
    };
    let cache = AssetResolver::new();
    let list = build_device_list(&[], &[], &cache, &Config::default(), &[camera]);
    // Port-stable even without a serial: settings follow the model, not the
    // OS capture id (which embeds the USB location on macOS/Windows).
    assert_eq!(list[0].config_key, "camera:046d:082d");
    assert_eq!(list[0].capture_id.as_deref(), Some("0x14110000046d082d"));
}

#[test]
fn webcam_config_key_survives_a_usb_port_change() {
    let port_a = Camera {
        name: "Logitech StreamCam".to_string(),
        unique_id: "0x1123000046d0893".to_string(),
        serial_number: Some("SN42".to_string()),
        vendor_id: 0x046d,
        product_id: 0x0893,
        max_resolution: None,
        max_fps: None,
    };
    let port_b = Camera {
        unique_id: "0x14110000046d0893".to_string(),
        ..port_a.clone()
    };
    let cache = AssetResolver::new();
    let a = build_device_list(&[], &[], &cache, &Config::default(), &[port_a]);
    let b = build_device_list(&[], &[], &cache, &Config::default(), &[port_b]);
    assert_eq!(a[0].config_key, b[0].config_key);
    assert_eq!(a[0].record_key(), b[0].record_key());
    assert_ne!(a[0].capture_id, b[0].capture_id);
}

#[test]
fn two_serial_less_same_model_cameras_stay_distinct() {
    // Hardware settings share the model key (no USB serial to go on), but
    // inventory and user-facing identity use capture_id so both remain
    // independently selectable and nameable.
    let a = Camera {
        name: "Logitech StreamCam".to_string(),
        unique_id: "0x1123000046d0893".to_string(),
        serial_number: None,
        vendor_id: 0x046d,
        product_id: 0x0893,
        max_resolution: None,
        max_fps: None,
    };
    let b = Camera {
        unique_id: "0x14110000046d0893".to_string(),
        ..a.clone()
    };
    let cache = AssetResolver::new();
    let list = build_device_list(&[], &[], &cache, &Config::default(), &[a, b]);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].config_key, list[1].config_key);
    assert_eq!(list[0].config_key, "camera:046d:0893");
    assert_ne!(list[0].inventory_key(), list[1].inventory_key());
    assert_ne!(list[0].record_key(), list[1].record_key());
    assert_ne!(list[0].capture_id, list[1].capture_id);
}
