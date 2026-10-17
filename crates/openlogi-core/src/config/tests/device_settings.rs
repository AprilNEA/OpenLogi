//! Per-device settings: DPI and its presets, SmartShift, scroll inversion and resolution, the smooth wheel, keeping the OS layout, the report rate, and the stored bindings.

use super::*;

#[test]
fn dpi_roundtrips_per_device() {
    let mut cfg = Config::default();
    cfg.set_dpi("2b042", Dpi::new(1600));
    let restored = write_and_read(&cfg);
    assert_eq!(restored.dpi("2b042"), Some(Dpi::new(1600)));
    assert_eq!(restored.dpi("absent"), None);
}

#[test]
fn smartshift_roundtrips_per_device() {
    let mut cfg = Config::default();
    let smartshift = SmartShift {
        mode: WheelMode::Ratchet,
        auto_disengage: SmartShiftAutoDisengage::Threshold(
            SmartShiftThreshold::try_new(16).expect("valid threshold"),
        ),
        tunable_torque: Some(TunableTorque::try_new(30).expect("valid torque")),
    };
    cfg.set_smartshift("2b042", smartshift);
    let restored = write_and_read(&cfg);
    assert_eq!(restored.smartshift("2b042"), Some(smartshift));
    assert_eq!(restored.smartshift("absent"), None);
}

#[test]
fn invert_scroll_roundtrips_per_device() {
    let mut cfg = Config::default();
    // Default is the native direction for any device, present or not.
    assert!(!cfg.invert_scroll("2b042"));
    cfg.set_invert_scroll("2b042", true);
    let restored = write_and_read(&cfg);
    assert!(restored.invert_scroll("2b042"));
    assert!(!restored.invert_scroll("absent"));
}

#[test]
fn default_invert_scroll_is_omitted_from_toml() {
    // A device block with only the default (false) invert_scroll must not
    // emit the field — `skip_serializing_if` keeps configs clean.
    let mut cfg = Config::default();
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.set_invert_scroll("2b042", false);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("invert_scroll"),
        "default invert_scroll should be omitted: {body}"
    );
}

#[test]
fn keep_os_layout_defaults_on_and_roundtrips_per_device() {
    let mut cfg = Config::default();
    // On for any device until the user turns it off, as in Logi Options+.
    assert!(cfg.keep_os_layout("2b042"));
    cfg.set_keep_os_layout("2b042", false);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(body.contains("keep_os_layout = false"), "{body}");
    let restored = write_and_read(&cfg);
    assert!(!restored.keep_os_layout("2b042"));
    assert!(restored.keep_os_layout("absent"));
    cfg.set_keep_os_layout("2b042", true);
    assert!(write_and_read(&cfg).keep_os_layout("2b042"));
}

#[test]
fn unset_keep_os_layout_is_omitted_from_toml() {
    let mut cfg = Config::default();
    cfg.set_invert_scroll("2b042", true);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("keep_os_layout"), "{body}");
}

#[test]
fn smooth_wheel_defaults_off_and_roundtrips_per_device() {
    let mut cfg = Config::default();
    assert!(!cfg.smooth_wheel("mouse"));
    cfg.set_smooth_wheel("mouse", true);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(body.contains("smooth_wheel = true"), "{body}");
    let restored = write_and_read(&cfg);
    assert!(restored.smooth_wheel("mouse"));
    assert!(!restored.smooth_wheel("absent"));
    cfg.set_smooth_wheel("mouse", false);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("smooth_wheel"), "off is omitted: {body}");
    assert!(!write_and_read(&cfg).smooth_wheel("mouse"));
}

#[test]
fn a_smooth_wheel_opt_in_survives_route_adoption() {
    let route = "receiver:82839805:slot:2";
    let mut config = Config::default();
    config.set_smooth_wheel(route, true);
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:4f4c4403").expect("valid");
    assert!(config.adopt_route(&canonical, route, None));
    assert!(
        !config.devices.contains_key(route),
        "sanity: the route entry folded"
    );
    assert!(config.smooth_wheel("unit:4f4c4403"));
}

#[test]
fn scroll_resolution_roundtrips_all_three_states() {
    let mut cfg = Config::default();
    assert_eq!(cfg.scroll_resolution("mouse"), None);

    cfg.set_scroll_resolution("mouse", Some(ScrollResolution::Low));
    let low = write_and_read(&cfg);
    assert_eq!(low.scroll_resolution("mouse"), Some(ScrollResolution::Low));

    cfg.set_scroll_resolution("mouse", Some(ScrollResolution::High));
    let high = write_and_read(&cfg);
    assert_eq!(
        high.scroll_resolution("mouse"),
        Some(ScrollResolution::High)
    );

    cfg.set_scroll_resolution("mouse", None);
    let unmanaged = write_and_read(&cfg);
    assert_eq!(unmanaged.scroll_resolution("mouse"), None);
}

#[test]
fn unset_scroll_resolution_is_omitted_from_toml() {
    let mut cfg = Config::default();
    cfg.set_binding("mouse", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.set_scroll_resolution("mouse", Some(ScrollResolution::Low));
    cfg.set_scroll_resolution("mouse", None);

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("scroll_resolution"),
        "unset scroll resolution should be omitted: {body}"
    );
}

#[test]
fn config_without_scroll_resolution_loads_as_unmanaged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r"
            schema_version = 3
            [devices.mouse]
            invert_scroll = true
        ",
    )
    .expect("write config");

    let cfg = Config::load_from_path(&path).expect("load existing config");
    assert_eq!(cfg.scroll_resolution("mouse"), None);
    assert!(cfg.invert_scroll("mouse"));
}

#[test]
fn bindings_roundtrip_per_device() {
    let mut cfg = Config::default();
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.set_binding(
        "2b042",
        ButtonId::DpiToggle,
        Binding::Single(Action::CustomShortcut(
            "Cmd+P".parse().expect("valid shortcut failed"),
        )),
    );
    cfg.set_binding("4082d", ButtonId::Back, Binding::Single(Action::Paste));

    let parsed = write_and_read(&cfg);

    // Per-device isolation.
    let a = parsed.stored_bindings("2b042");
    assert_eq!(a.get(&ButtonId::Back), Some(&Binding::Single(Action::Copy)));
    assert_eq!(
        a.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::CustomShortcut(
            "Cmd+P".parse().expect("valid shortcut failed")
        )))
    );

    let b = parsed.stored_bindings("4082d");
    assert_eq!(
        b.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Paste))
    );
    assert_eq!(b.len(), 1, "device b should only see its own bindings");

    // Unknown device returns empty map without panic.
    assert!(parsed.stored_bindings("deadbeef").is_empty());
}

#[test]
fn dpi_presets_roundtrip_per_device() {
    let mut cfg = Config::default();
    cfg.set_dpi_presets("2b042", vec![Dpi::new(800), Dpi::new(1600), Dpi::new(3200)]);
    cfg.set_dpi_presets("4082d", vec![Dpi::new(400), Dpi::new(1600)]);

    let parsed = write_and_read(&cfg);

    assert_eq!(
        parsed.dpi_presets("2b042"),
        vec![Dpi::new(800), Dpi::new(1600), Dpi::new(3200)]
    );
    assert_eq!(
        parsed.dpi_presets("4082d"),
        vec![Dpi::new(400), Dpi::new(1600)]
    );
    assert!(parsed.dpi_presets("unknown").is_empty());
}

#[test]
fn empty_dpi_presets_skip_serialization() {
    let mut cfg = Config::default();
    // Add a binding so the device block exists.
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.set_dpi_presets("2b042", vec![Dpi::new(800)]);
    cfg.set_dpi_presets("2b042", vec![]); // clear

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("dpi_presets"),
        "empty dpi_presets should be omitted: {body}"
    );
}

/// `KeyScreenCapture` and `control:0x010a` are one control. TOML cannot see
/// that collision, so the config layer must — a last-wins map would drop one
/// binding and save the loss.
#[test]
fn a_control_named_under_two_spellings_is_rejected() {
    let device_table = format!(
        "schema_version = {SCHEMA_VERSION}\n\
         [devices.\"unit:1\".bindings]\n\
         KeyScreenCapture = \"Sleep\"\n\
         \"control:0x010a\" = \"Copy\"\n"
    );
    let err = toml::from_str::<Config>(&device_table)
        .expect_err("one control bound under two names must not load");
    assert!(
        err.to_string()
            .contains("`KeyScreenCapture` and `control:0x010a`"),
        "{err}"
    );

    let per_app_table = format!(
        "schema_version = {SCHEMA_VERSION}\n\
         [devices.\"unit:1\".per_app_bindings.\"com.example.App\"]\n\
         \"control:0x010a\" = \"Copy\"\n\
         KeyScreenCapture = \"Sleep\"\n"
    );
    toml::from_str::<Config>(&per_app_table).expect_err("the guard covers per-app overlays too");

    // Distinct controls under mixed spellings still load.
    let mixed = format!(
        "schema_version = {SCHEMA_VERSION}\n\
         [devices.\"unit:1\".bindings]\n\
         KeyScreenCapture = \"Sleep\"\n\
         \"control:0x01f3\" = \"Copy\"\n"
    );
    let config: Config = toml::from_str(&mixed).expect("distinct controls load");
    assert_eq!(config.stored_bindings("unit:1").len(), 2);
}

#[test]
fn a_keep_os_layout_opt_out_survives_route_adoption() {
    // The CLI can save to a route-keyed entry; the app later adopts that
    // route into the keyboard's identity-keyed entry. The opt-out must come
    // along, or the default turns the layout write back on.
    let route = "receiver:82839805:slot:1";
    let mut config = Config::default();
    config.set_keep_os_layout(route, false);
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:6be9d300").expect("valid");
    assert!(config.adopt_route(&canonical, route, None));
    assert!(
        !config.devices.contains_key(route),
        "sanity: the route entry folded"
    );
    assert!(!config.keep_os_layout("unit:6be9d300"));
}

#[test]
fn report_rate_roundtrips_per_device_in_hertz() {
    use crate::hid::ReportRate;

    let mut cfg = Config::default();
    cfg.set_report_rate("2b042", ReportRate::new(1000));
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(
        toml.contains("report_rate = 1000"),
        "the rate is stored as plain hertz: {toml}"
    );
    let restored = write_and_read(&cfg);
    assert_eq!(restored.report_rate("2b042"), Some(ReportRate::new(1000)));
    assert_eq!(restored.report_rate("absent"), None);
}

#[test]
fn unset_report_rate_is_omitted_from_toml() {
    let mut cfg = Config::default();
    cfg.set_dpi("2b042", Dpi::new(1600));
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(!toml.contains("report_rate"), "{toml}");
}

#[test]
fn a_saved_report_rate_survives_route_adoption() {
    // The CLI can save to a route-keyed entry; the app later adopts that
    // route into the mouse's identity-keyed entry. The rate must come along,
    // or the next reconnect stops re-applying it.
    use crate::hid::ReportRate;

    let route = "receiver:82839805:slot:2";
    let mut config = Config::default();
    config.set_report_rate(route, ReportRate::new(500));
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:a393cae0").expect("valid");
    assert!(config.adopt_route(&canonical, route, None));
    assert!(
        !config.devices.contains_key(route),
        "sanity: the route entry folded"
    );
    assert_eq!(
        config.report_rate("unit:a393cae0"),
        Some(ReportRate::new(500))
    );
}
