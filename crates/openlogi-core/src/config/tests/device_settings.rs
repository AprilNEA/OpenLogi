//! Per-device settings: DPI and its presets, SmartShift, scroll inversion and resolution, and the stored bindings.

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
fn onboard_memory_and_report_rate_roundtrip() {
    let mut cfg = Config::default();
    cfg.set_onboard_memory("unit:1", Some(OnboardMemory::Profile(5)));
    cfg.set_report_rate("unit:1", ReportRate::from_ms(1).expect("valid rate"));
    cfg.set_onboard_memory("unit:2", Some(OnboardMemory::Off));
    let restored = write_and_read(&cfg);
    assert_eq!(
        restored.onboard_memory("unit:1"),
        Some(OnboardMemory::Profile(5))
    );
    assert_eq!(restored.onboard_memory("unit:2"), Some(OnboardMemory::Off));
    assert_eq!(restored.report_rate("unit:1").map(ReportRate::ms), Some(1));
}

#[test]
fn host_mode_is_implied_only_by_host_only_buttons_or_report_rate() {
    let layout = Some(&G502_LIGHTSPEED);
    let mut cfg = Config::default();
    assert_eq!(cfg.effective_onboard_memory("g502", layout), None);

    cfg.set_binding("g502", ButtonId::Back, Binding::Single(Action::Copy));
    assert_eq!(cfg.effective_onboard_memory("g502", layout), None);

    cfg.set_binding(
        "g502",
        ButtonId::G7,
        Binding::Single(Action::PreviousDpiPreset),
    );
    assert_eq!(cfg.effective_onboard_memory("g502", layout), None);

    cfg.set_binding("g502", ButtonId::G7, Binding::Single(Action::Copy));
    assert_eq!(
        cfg.effective_onboard_memory("g502", layout),
        Some(OnboardMemory::Off)
    );
    assert_eq!(cfg.effective_onboard_memory("g502", None), None);

    cfg.set_onboard_memory("g502", Some(OnboardMemory::Profile(1)));
    assert_eq!(
        cfg.effective_onboard_memory("g502", layout),
        Some(OnboardMemory::Profile(1))
    );

    let mut rate_only = Config::default();
    rate_only.set_report_rate("g502", ReportRate::from_ms(2).expect("valid rate"));
    assert_eq!(
        rate_only.effective_onboard_memory("g502", layout),
        Some(OnboardMemory::Off)
    );
}

#[test]
fn gshift_layer_roundtrips_and_needs_host_mode() {
    let layout = Some(&G502_LIGHTSPEED);
    let mut cfg = Config::default();
    cfg.set_binding("g502", ButtonId::Forward, Binding::Single(Action::GShift));
    assert!(cfg.uses_gshift("g502"));
    assert_eq!(
        cfg.effective_onboard_memory("g502", layout),
        Some(OnboardMemory::Off)
    );

    cfg.set_gshift_binding("g502", ButtonId::G8, Some(Action::VolumeUp));
    let restored = write_and_read(&cfg);
    assert_eq!(
        restored
            .gshift_overrides("g502")
            .and_then(|layer| layer.get(&ButtonId::G8)),
        Some(&Action::VolumeUp)
    );

    cfg.set_gshift_binding("g502", ButtonId::G8, None);
    assert_eq!(cfg.gshift_overrides("g502"), None);
}
