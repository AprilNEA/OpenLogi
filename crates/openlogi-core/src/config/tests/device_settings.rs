//! Per-device settings: DPI and its presets, SmartShift, scroll inversion and resolution, the smooth wheel and its momentum scrolling, keeping the OS layout, the report rate, the camera preview's mode, and the stored bindings.

use super::*;
use std::collections::BTreeSet;

#[test]
fn dpi_roundtrips_per_device() {
    let mut cfg = Config::default();
    cfg.set_dpi("2b042", Dpi::new(1600));
    let restored = write_and_read(&cfg);
    assert_eq!(restored.dpi("2b042"), Some(Dpi::new(1600)));
    assert_eq!(restored.dpi("absent"), None);
}

#[test]
fn disabled_keys_roundtrip_and_clear_per_device() {
    let mut cfg = Config::default();
    let desired = BTreeSet::from([DisableKey::CapsLock, DisableKey::WindowsCommand]);

    assert_eq!(cfg.disabled_keys("keyboard"), None);
    cfg.set_disabled_keys("keyboard", desired.clone());
    let restored = write_and_read(&cfg);
    assert_eq!(restored.disabled_keys("keyboard"), Some(&desired));

    cfg.clear_disabled_keys("keyboard");
    assert_eq!(cfg.disabled_keys("keyboard"), None);
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
fn momentum_scrolling_overrides_per_device_and_falls_back_to_the_app_wide_value() {
    use crate::config::MomentumScrolling;
    let mut cfg = Config::default();
    cfg.app_settings.momentum_scrolling.enabled = true;
    assert_eq!(
        cfg.momentum_scrolling("mouse"),
        cfg.app_settings.momentum_scrolling
    );
    let own = MomentumScrolling {
        enabled: false,
        glide_ms: 800,
        ..MomentumScrolling::default()
    };
    cfg.set_device_momentum_scrolling("mouse", Some(own));
    let restored = write_and_read(&cfg);
    assert_eq!(restored.momentum_scrolling("mouse"), own);
    assert!(
        restored.momentum_scrolling("absent").enabled,
        "a device without an override follows the app-wide value"
    );
    cfg.set_device_momentum_scrolling("mouse", None);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("[devices.mouse.momentum_scrolling]"),
        "a cleared override is omitted: {body}"
    );
    assert!(write_and_read(&cfg).momentum_scrolling("mouse").enabled);
}

#[test]
fn a_momentum_override_survives_route_adoption() {
    use crate::config::MomentumScrolling;
    let route = "receiver:82839805:slot:2";
    let mut config = Config::default();
    let own = MomentumScrolling {
        enabled: true,
        ..MomentumScrolling::default()
    };
    config.set_device_momentum_scrolling(route, Some(own));
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:4f4c4403").expect("valid");
    assert!(config.adopt_route(&canonical, route, None));
    assert_eq!(config.momentum_scrolling("unit:4f4c4403"), own);
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
fn pointer_speed_roundtrips_as_the_raw_factor() {
    use crate::hid::PointerSpeed;
    let mut cfg = Config::default();
    let speed = PointerSpeed::try_new(0x0180).expect("in range");
    cfg.set_pointer_speed("2b042", speed);
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(
        toml.contains("pointer_speed = 384"),
        "the speed is stored as its 1/256 factor: {toml}"
    );
    let restored = write_and_read(&cfg);
    assert_eq!(restored.pointer_speed("2b042"), Some(speed));
    assert_eq!(restored.pointer_speed("absent"), None);
}

#[test]
fn a_pointer_speed_outside_the_safe_range_does_not_load() {
    // The firmware takes any 16-bit factor and a tiny one stops the pointer
    // (#1372), so a hand-edited value outside 32..=768 is refused rather
    // than written to the mouse.
    toml::from_str::<Config>("[devices.2b042]\npointer_speed = 1\n")
        .expect_err("a pointer speed below 32 is refused");
}

#[test]
fn force_sensing_roundtrips_as_the_raw_device_value() {
    let mut cfg = Config::default();
    cfg.set_force_sensing("2b042", 4500);
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(
        toml.contains("force_sensing = 4500"),
        "the force is stored as the raw device value: {toml}"
    );
    let restored = write_and_read(&cfg);
    assert_eq!(restored.force_sensing("2b042"), Some(4500));
    assert_eq!(restored.force_sensing("absent"), None);
}

#[test]
fn unset_force_sensing_is_omitted_from_toml() {
    // Absent means the agent never writes the force, so it must not appear
    // just because another setting was saved.
    let mut cfg = Config::default();
    cfg.set_dpi("2b042", Dpi::new(1600));
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(!toml.contains("force_sensing"), "{toml}");
    assert_eq!(write_and_read(&cfg).force_sensing("2b042"), None);
}

#[test]
fn a_saved_force_sensing_survives_route_adoption() {
    let route = "receiver:82839805:slot:2";
    let mut config = Config::default();
    config.set_force_sensing(route, 6000);
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:a393cae0").expect("valid");
    assert!(config.adopt_route(&canonical, route, None));
    assert_eq!(config.force_sensing("unit:a393cae0"), Some(6000));
}

fn stream_mode(width: u32, height: u32, fps: u32) -> CameraStreamMode {
    let non_zero = |value| std::num::NonZeroU32::new(value).expect("non-zero");
    CameraStreamMode {
        width: non_zero(width),
        height: non_zero(height),
        fps: non_zero(fps),
    }
}

#[test]
fn camera_stream_mode_roundtrips_apart_from_the_camera_controls() {
    let key = "camera:046d:0893";
    let mut cfg = Config::default();
    cfg.set_camera_controls(key, CameraControls::default());
    cfg.set_camera_stream_mode(key, stream_mode(1920, 1080, 60));
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(toml.contains("camera_stream_mode"), "{toml}");

    let restored = write_and_read(&cfg);
    assert_eq!(
        restored.camera_stream_mode(key),
        Some(stream_mode(1920, 1080, 60))
    );
    assert_eq!(
        restored.camera_controls(key),
        Some(CameraControls::default()),
        "the mode adds nothing to the controls"
    );
    assert_eq!(restored.camera_stream_mode("camera:046d:085e"), None);
}

#[test]
fn unset_camera_stream_mode_is_omitted_from_toml() {
    // Absent means the preview opens at the camera's default, so saving a
    // control must not write a mode.
    let mut cfg = Config::default();
    cfg.set_camera_controls("camera:046d:0893", CameraControls::default());
    let toml = toml::to_string(&cfg).expect("serialize");
    assert!(!toml.contains("camera_stream_mode"), "{toml}");
    assert_eq!(
        write_and_read(&cfg).camera_stream_mode("camera:046d:0893"),
        None
    );
}

#[test]
fn a_camera_stream_mode_with_a_zero_or_a_missing_value_does_not_load() {
    let config = |mode: &str| {
        toml::from_str::<Config>(&format!(
            "schema_version = {SCHEMA_VERSION}\n[devices.cam.camera_stream_mode]\n{mode}\n"
        ))
    };
    let loaded = config("width = 1280\nheight = 720\nfps = 30").expect("a whole mode loads");
    assert_eq!(
        loaded.camera_stream_mode("cam"),
        Some(stream_mode(1280, 720, 30))
    );

    for mode in [
        "width = 0\nheight = 720\nfps = 30",
        "width = 1280\nheight = 720\nfps = 0",
        "width = 1280\nheight = 720",
    ] {
        config(mode).expect_err(mode);
    }
}

#[test]
fn a_saved_pointer_speed_survives_route_adoption() {
    use crate::hid::PointerSpeed;
    let route = "receiver:82839805:slot:2";
    let mut config = Config::default();
    config.set_pointer_speed(route, PointerSpeed::MAX);
    let canonical = crate::device_order::PhysicalDeviceKey::parse("unit:a393cae0").expect("valid");
    assert!(config.adopt_route(&canonical, route, None));
    assert_eq!(
        config.pointer_speed("unit:a393cae0"),
        Some(PointerSpeed::MAX)
    );
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

/// A profile is a plain number in the file, and the mouse has no profile 0.
#[test]
fn onboard_profile_is_a_nonzero_number_in_the_file() {
    let body = |profile: u8| {
        format!("schema_version = 7\n[devices.\"unit:1\".onboard_memory]\nprofile = {profile}\n")
    };
    let parsed: Config = toml::from_str(&body(3)).expect("profile 3 loads");
    assert_eq!(
        parsed.onboard_memory("unit:1"),
        Some(OnboardMemory::Profile(
            crate::hid::ProfileIndex::new(3).unwrap()
        ))
    );
    assert!(
        toml::from_str::<Config>(&body(0)).is_err(),
        "profile 0 is refused when the file loads"
    );
}

#[test]
fn onboard_memory_and_report_rate_roundtrip() {
    let mut cfg = Config::default();
    cfg.set_onboard_memory(
        "unit:1",
        Some(OnboardMemory::Profile(
            crate::hid::ProfileIndex::new(5).unwrap(),
        )),
    );
    cfg.set_report_rate("unit:1", ReportRate::new(1000));
    cfg.set_onboard_memory("unit:2", Some(OnboardMemory::Off));
    let restored = write_and_read(&cfg);
    assert_eq!(
        restored.onboard_memory("unit:1"),
        Some(OnboardMemory::Profile(
            crate::hid::ProfileIndex::new(5).unwrap()
        ))
    );
    assert_eq!(restored.onboard_memory("unit:2"), Some(OnboardMemory::Off));
    assert_eq!(restored.report_rate("unit:1"), Some(ReportRate::new(1000)));
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

    cfg.set_onboard_memory(
        "g502",
        Some(OnboardMemory::Profile(
            crate::hid::ProfileIndex::new(1).unwrap(),
        )),
    );
    assert_eq!(
        cfg.effective_onboard_memory("g502", layout),
        Some(OnboardMemory::Profile(
            crate::hid::ProfileIndex::new(1).unwrap()
        ))
    );

    let mut rate_only = Config::default();
    rate_only.set_report_rate("g502", ReportRate::new(500));
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

#[test]
fn gshift_is_held_only_by_a_plain_binding() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "g502",
        ButtonId::Forward,
        Binding::LongPress(crate::binding::LongPressBinding::new(
            Action::GShift,
            Action::Copy,
        )),
    );
    assert!(
        !cfg.uses_gshift("g502"),
        "a long press keeps its long action"
    );
    cfg.set_binding("g502", ButtonId::Forward, Binding::Single(Action::GShift));
    assert!(cfg.uses_gshift("g502"));
}
