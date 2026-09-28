//! The link table: measured capabilities, overrides, and an empty table.

use super::*;

#[test]
fn a_config_without_links_round_trips_unchanged() {
    // Every existing file has no `links`; serializing must not start writing
    // an empty table into files that never had one.
    let source =
        "schema_version = 4\n\n[devices.\"receiver:82839805:slot:1\"]\ninvert_scroll = true\n";
    let config: Config = toml::from_str(source).expect("parses");
    let device = config
        .devices
        .get("receiver:82839805:slot:1")
        .expect("entry");
    assert!(device.links.is_empty());
    let written = toml::to_string(&config).expect("serializes");
    assert!(!written.contains("links"), "got: {written}");
}

#[test]
fn a_link_carries_measured_capabilities_and_overrides() {
    let source = r#"
schema_version = 5

[devices."unit:6be9d300".links."direct:046d:c08d".capabilities]
buttons = false
pointer = true
lighting = true
scroll_inversion = false
hires_wheel = false
thumbwheel = false
haptic_feedback = false
haptic_panel = false

[devices."unit:6be9d300".links."direct:046d:c08d".overrides]
dpi = 1600
invert_scroll = true
"#;
    let config: Config = toml::from_str(source).expect("parses");
    let link = config.devices["unit:6be9d300"].links["direct:046d:c08d"].clone();
    assert!(!link.capabilities.expect("measured").hires_wheel);
    assert_eq!(link.overrides.dpi, Some(Dpi::new(1600)));
    assert_eq!(link.overrides.invert_scroll, Some(true));
}

#[test]
fn an_empty_link_table_survives_a_save_and_reload() {
    // The set of `links` keys *is* the route index — the only thing that can
    // identify a sleeping device from its route alone. A link with nothing
    // special about it is an empty sub-table, so if serialization or the
    // comment-preserving `reconcile_table` merge ever drops one, the index
    // quietly empties on every save and every sleeping device falls back to
    // its route key.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "schema_version = 5\n\n# a hand-written comment\n[devices.\"unit:6be9d300\"]\ndpi = 1600\n",
    )
    .expect("write v5 config");

    let mut config = Config::load_from_path(&path).expect("load");
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
    assert!(config.adopt_route(&canonical, "receiver:82839805:slot:1", None));
    assert!(
        config.devices["unit:6be9d300"].links["receiver:82839805:slot:1"]
            .overrides
            .is_empty(),
        "sanity: the link carries nothing but its own existence"
    );
    config.save_to_path(&path).expect("save");

    let reloaded = Config::load_from_path(&path).expect("reload");
    assert!(
        reloaded.devices["unit:6be9d300"]
            .links
            .contains_key("receiver:82839805:slot:1"),
        "the route index survives the round trip: {}",
        fs::read_to_string(&path).expect("read back")
    );
}

#[test]
fn a_link_override_shadows_the_device_value() {
    let mut device = DeviceConfig {
        dpi: Some(Dpi::new(1600)),
        ..DeviceConfig::default()
    };
    device.links.insert(
        "receiver:82839805:slot:1".to_string(),
        LinkConfig {
            capabilities: None,
            overrides: LinkOverrides {
                dpi: Some(Dpi::new(800)),
                ..LinkOverrides::default()
            },
        },
    );

    assert_eq!(
        device.effective_dpi("receiver:82839805:slot:1"),
        Some(Dpi::new(800)),
        "the link the user set it on wins"
    );
    assert_eq!(
        device.effective_dpi("direct:046d:c08d"),
        Some(Dpi::new(1600)),
        "every other link keeps the device default"
    );
}

#[test]
fn an_unknown_route_gets_the_device_value() {
    // A device seen on a route with no entry yet must not lose its settings.
    let device = DeviceConfig {
        dpi: Some(Dpi::new(1600)),
        ..DeviceConfig::default()
    };
    assert_eq!(
        device.effective_dpi("direct:046d:ffff"),
        Some(Dpi::new(1600))
    );
}
