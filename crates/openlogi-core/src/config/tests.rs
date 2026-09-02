//! Config load/save and binding-map tests.

use std::{assert_matches, fs};

use super::*;
use crate::binding::{default_binding, default_gesture_binding};
use crate::hid::{Dpi, SmartShiftAutoDisengage, SmartShiftThreshold, TunableTorque};

fn write_and_read(config: &Config) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    config.save_to_path(&path).expect("save");
    Config::load_from_path(&path).expect("load")
}

#[test]
fn canonical_configuration_example_parses() {
    let body = include_str!("../../../../docs/config.example.toml");
    let config: Config = toml::from_str(body).expect("documented config must parse");
    assert_eq!(config.schema_version, SCHEMA_VERSION);
    let bindings = config.bindings_for("receiver:aabbccdd:slot:1");
    let Some(Binding::LongPress(long_press)) = bindings.get(&ButtonId::DpiToggle) else {
        panic!("documented long-press binding should keep its shape");
    };
    assert_eq!(long_press.short(), &Action::ShowDesktop);
    assert_eq!(long_press.long(), &Action::MissionControl);
}

#[test]
fn first_save_preserves_the_previous_config_for_recovery() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let backup = dir.path().join("config.toml.backup.1");
    let original = b"schema_version = 3\nselected_device = \"original\"\n";
    fs::write(&path, original).expect("write original config");

    let mut config = Config {
        selected_device: Some("replacement".to_string()),
        ..Config::default()
    };
    config.save_to_path(&path).expect("save replacement");
    assert_eq!(fs::read(&backup).expect("read backup"), original);

    config.selected_device = Some("second-save".to_string());
    config.save_to_path(&path).expect("save again");
    assert_eq!(
        fs::read(&backup).expect("read original backup"),
        original,
        "later saves in one process must not replace the recovery copy"
    );
}

#[test]
fn migrated_load_backs_up_the_pre_migration_source_exactly_once() {
    // A key-rewriting migration touches every entry, so the pre-migration
    // file is the user's only recovery path if the rewrite is wrong. The
    // backup must hold the source exactly as loaded — not the migrated,
    // re-serialized output — and must not be retaken on a later save from
    // the same `ConfigFile` (`migrated_from` is consumed with `Option::take`).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    // Appended to the full file name, not substituted for its extension:
    // `config.toml` + `.v4.bak`, never `config.v4.bak`.
    let backup = dir.path().join("config.toml.v4.bak");
    let original =
        b"schema_version = 4\n\n[devices.\"direct:046d:c08d:unit:6be9d300\"]\ninvert_scroll = true\n";
    fs::write(&path, original).expect("write v4 config");

    let (config, mut file) = ConfigFile::load_from_path(&path).expect("load v4 config");
    assert!(
        config.devices.contains_key("unit:6be9d300"),
        "sanity: the key migration ran"
    );

    file.save(&config).expect("save migrated config");
    assert_eq!(
        fs::read(&backup).expect("read migration backup"),
        original,
        "the backup preserves the pre-migration source, not the rewritten output"
    );

    // Replace the backup with a sentinel that `original` never equals, so a
    // second write is observable even though it would write the same bytes
    // as the first: re-asserting against `original` here cannot distinguish
    // "not rewritten" from "rewritten with identical content", which is
    // exactly the regression this test exists to catch (`migrated_from`
    // going from a consuming `Option::take` to a plain read).
    let sentinel = b"sentinel: a second save must not touch this file";
    fs::write(&backup, sentinel).expect("overwrite backup with sentinel");

    let mut second = config.clone();
    second.selected_device = Some("unit:6be9d300".to_string());
    file.save(&second)
        .expect("save again from the same ConfigFile");
    assert_eq!(
        fs::read(&backup).expect("read backup after second save"),
        sentinel,
        "a second save from the same ConfigFile must not rewrite the backup"
    );
}

#[test]
fn config_backups_rotate_between_generations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, b"first").expect("write first generation");
    super::backup_existing_config(&path).expect("back up first generation");

    fs::write(&path, b"second").expect("write second generation");
    super::backup_existing_config(&path).expect("back up second generation");

    assert_eq!(
        fs::read(super::config_backup_path(&path, 1).expect("backup path"))
            .expect("read newest backup"),
        b"second"
    );
    assert_eq!(
        fs::read(super::config_backup_path(&path, 2).expect("backup path"))
            .expect("read older backup"),
        b"first"
    );
}

#[test]
fn key_trigger_parses_bare_and_modified() {
    // Bare function key — F1 is macOS keycode 0x7A.
    let t: KeyTrigger = "f1".parse().expect("parse key trigger");
    assert_eq!(t.keycode, 0x7A);
    assert!(t.modifiers.is_empty());

    // Modifier-qualified, in any order, with aliases.
    let t: KeyTrigger = "shift+cmd+f5".parse().expect("parse key trigger");
    assert_eq!(t.keycode, 0x60); // F5
    assert!(t.modifiers.shift && t.modifiers.command);
    assert!(!t.modifiers.control && !t.modifiers.option);

    let t: KeyTrigger = "ctrl+alt+f2".parse().expect("parse key trigger");
    assert!(t.modifiers.control && t.modifiers.option);

    // Esc.
    assert_eq!(
        "esc"
            .parse::<KeyTrigger>()
            .expect("parse key trigger")
            .keycode,
        0x35
    );
}

#[test]
fn key_trigger_parses_and_displays_extended_function_keys() {
    let f13: KeyTrigger = "f13".parse().expect("parse key trigger");
    let f17: KeyTrigger = "command+f17".parse().expect("parse key trigger");
    let f19: KeyTrigger = "f19".parse().expect("parse key trigger");

    assert_eq!(f13.keycode, 0x69);
    assert_eq!(f17.keycode, 0x40);
    assert_eq!(f17.to_string(), "command+f17");
    assert_eq!(f19.keycode, 0x50);
    assert_eq!(f19.to_string(), "f19");
}

#[test]
fn key_trigger_rejects_unknown() {
    "f99"
        .parse::<KeyTrigger>()
        .expect_err("f99 is not a known key name");
    "shift+"
        .parse::<KeyTrigger>()
        .expect_err("a modifier with no key must be rejected");
    "".parse::<KeyTrigger>()
        .expect_err("an empty trigger must be rejected");
}

#[test]
fn keyboard_section_roundtrips_through_config() {
    let mut config = Config::default();
    config.keyboard.bindings.insert(
        "f1".parse().expect("parse key trigger"),
        Action::TypeText("hello".into()),
    );
    config.keyboard.bindings.insert(
        "shift+f2".parse().expect("parse key trigger"),
        Action::VolumeUp,
    );
    config.keyboard.bindings.insert(
        "f17".parse().expect("parse key trigger"),
        Action::MissionControl,
    );

    let roundtripped = write_and_read(&config);
    assert_eq!(roundtripped.keyboard.bindings.len(), 3);
    assert_eq!(
        roundtripped
            .keyboard
            .bindings
            .get(&"f1".parse::<KeyTrigger>().expect("parse key trigger")),
        Some(&Action::TypeText("hello".into()))
    );
    assert_eq!(
        roundtripped
            .keyboard
            .bindings
            .get(&"f17".parse::<KeyTrigger>().expect("parse key trigger")),
        Some(&Action::MissionControl)
    );
}

#[test]
fn set_keyboard_binding_inserts_and_clears() {
    let mut config = Config::default();
    let f1: KeyTrigger = "f1".parse().expect("parse key trigger");

    // Insert.
    config.set_keyboard_binding(f1.clone(), Some(Action::VolumeUp));
    assert_eq!(config.keyboard_bindings().get(&f1), Some(&Action::VolumeUp));
    assert_eq!(config.keyboard_bindings().len(), 1);

    // Overwrite.
    config.set_keyboard_binding(f1.clone(), Some(Action::MuteVolume));
    assert_eq!(
        config.keyboard_bindings().get(&f1),
        Some(&Action::MuteVolume)
    );
    assert_eq!(config.keyboard_bindings().len(), 1);

    // Clear via None.
    config.set_keyboard_binding(f1.clone(), None);
    assert!(config.keyboard_bindings().get(&f1).is_none());
    assert!(config.keyboard_bindings().is_empty());
}

#[test]
fn missing_file_yields_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nonexistent.toml");
    let cfg = Config::load_from_path(&path).expect("load");
    assert_eq!(cfg.schema_version, SCHEMA_VERSION);
    assert!(cfg.devices.is_empty());
}

#[test]
fn lighting_roundtrips_per_device() {
    let mut cfg = Config::default();
    cfg.set_lighting(
        "g513",
        Lighting {
            enabled: true,
            color: "00aabb".parse().expect("valid hex"),
            brightness: 75,
        },
    );
    let restored = write_and_read(&cfg);
    assert_eq!(
        restored.lighting("g513"),
        Some(Lighting {
            enabled: true,
            color: "00aabb".parse().expect("valid hex"),
            brightness: 75,
        })
    );
    assert_eq!(restored.lighting("absent"), None);
}

#[test]
fn standalone_light_settings_roundtrip_per_device() {
    let mut cfg = Config::default();
    cfg.set_light(
        "raw:046d:c900:ff43:0202:serial:glow",
        LightSettings {
            enabled: false,
            auto_camera: false,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        },
    );
    let restored = write_and_read(&cfg);
    assert_eq!(
        restored.light("raw:046d:c900:ff43:0202:serial:glow"),
        Some(LightSettings {
            enabled: false,
            auto_camera: false,
            brightness_percent: 65,
            temperature_kelvin: Some(4600),
            color: None,
        })
    );
    assert_eq!(restored.light("absent"), None);
}

#[test]
fn standalone_light_brightness_outside_percentage_range_is_rejected() {
    let error = toml::from_str::<Config>(
        r"
            schema_version = 3
            [devices.glow.light]
            enabled = true
            brightness_percent = 255
        ",
    )
    .expect_err("out-of-range brightness must fail");
    assert!(error.to_string().contains("between 0 and 100"));
}

#[test]
fn standalone_light_camera_automation_roundtrips() {
    let mut cfg = Config::default();
    cfg.set_light(
        "raw:046d:c900:ff43:0202:serial:glow",
        LightSettings {
            enabled: true,
            auto_camera: true,
            brightness_percent: 80,
            temperature_kelvin: Some(5000),
            color: None,
        },
    );

    let restored = write_and_read(&cfg);
    assert_eq!(
        restored
            .light("raw:046d:c900:ff43:0202:serial:glow")
            .map(|light| light.auto_camera),
        Some(true)
    );
}

#[test]
fn unparseable_lighting_color_is_rejected() {
    let error = toml::from_str::<Config>(
        r#"
            schema_version = 3
            [devices.g513.lighting]
            enabled = true
            color = "red"
            brightness = 50
        "#,
    )
    .expect_err("invalid RGB must fail");
    assert!(error.to_string().contains("invalid RGB color"));
}

#[test]
fn hash_prefixed_lighting_color_migrates_to_canonical_hex() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r##"
            schema_version = 3
            [devices.g513.lighting]
            enabled = true
            color = "#ff0000"
            brightness = 50
        "##,
    )
    .expect("write config");

    let cfg = Config::load_from_path(&path).expect("load hash-prefixed color");
    assert_eq!(
        cfg.lighting("g513").map(|lighting| lighting.color),
        Some(crate::color::Rgb::new(0xff, 0x00, 0x00))
    );

    cfg.save_to_path(&path).expect("save canonical color");
    let saved = fs::read_to_string(path).expect("read saved config");
    assert!(saved.contains("color = \"ff0000\""));
    assert!(!saved.contains("color = \"#"));
}

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
    let a = parsed.bindings_for("2b042");
    assert_eq!(a.get(&ButtonId::Back), Some(&Binding::Single(Action::Copy)));
    assert_eq!(
        a.get(&ButtonId::DpiToggle),
        Some(&Binding::Single(Action::CustomShortcut(
            "Cmd+P".parse().expect("valid shortcut failed")
        )))
    );

    let b = parsed.bindings_for("4082d");
    assert_eq!(
        b.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Paste))
    );
    assert_eq!(b.len(), 1, "device b should only see its own bindings");

    // Unknown device returns empty map without panic.
    assert!(parsed.bindings_for("deadbeef").is_empty());
}

#[test]
fn human_readable_toml_layout() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    let body = toml::to_string_pretty(&cfg).expect("serialize");

    // The key only contains [A-Za-z0-9_], so TOML emits it as a bare-word
    // table key (no surrounding quotes). The test asserts the observable
    // structure rather than locking in a specific quoting.
    assert!(
        body.contains(&format!("schema_version = {SCHEMA_VERSION}")),
        "got: {body}"
    );
    assert!(body.contains("[devices.2b042.bindings]"), "got: {body}");
    // A `Single` binding serializes byte-identically to the pre-v2 bare
    // `Action`, so the leaf line is unchanged.
    assert!(body.contains("Back = \"BrowserBack\""), "got: {body}");
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

#[test]
fn device_identity_roundtrips_and_is_iterable() {
    use crate::device::{Capabilities, DeviceKind};

    let mut cfg = Config::default();
    let mouse = DeviceIdentity {
        display_name: "MX Master 3S".to_string(),
        model_info: None,
        codename: None,
        kind: DeviceKind::Mouse,
        capabilities: Capabilities {
            buttons: true,
            pointer: true,
            lighting: false,
            scroll_inversion: false,
            hires_wheel: true,
            thumbwheel: false,
            haptic_feedback: false,
            haptic_panel: false,
        },
        light_capabilities: None,
        driver_id: None,
        registry_model_id: None,
    };
    cfg.set_device_identity("2b034", mouse.clone());
    // Recording an identity must not disturb unrelated per-device state.
    cfg.set_binding(
        "2b034",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );

    let parsed = write_and_read(&cfg);
    assert_eq!(parsed.device_identity("2b034"), Some(&mouse));
    assert_eq!(parsed.device_identity("absent"), None);
    assert_eq!(
        parsed.bindings_for("2b034").get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack)),
        "identity must coexist with bindings on the same device block"
    );
    assert_eq!(
        parsed.known_identities().collect::<Vec<_>>(),
        vec![("2b034", &mouse)]
    );
}

#[test]
fn custom_device_name_roundtrips_without_changing_model_identity() {
    use crate::device::{Capabilities, DeviceKind};

    let mut config = Config::default();
    config.set_device_identity(
        "receiver:test:slot:1",
        DeviceIdentity {
            display_name: "MX Master 4".into(),
            model_info: None,
            codename: None,
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::default(),
            light_capabilities: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    config.set_device_custom_name("receiver:test:slot:1", Some("Office".into()));

    let parsed = write_and_read(&config);

    assert_eq!(
        parsed.device_custom_name("receiver:test:slot:1"),
        Some("Office")
    );
    assert_eq!(
        parsed
            .device_identity("receiver:test:slot:1")
            .map(|identity| identity.display_name.as_str()),
        Some("MX Master 4")
    );
}

#[test]
fn persisted_identity_strips_per_unit_identifiers() {
    use crate::device::{Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports};

    let mut config = Config::default();
    config.set_device_identity(
        "receiver:test:slot:1",
        DeviceIdentity {
            display_name: "Mouse".into(),
            model_info: Some(DeviceModelInfo {
                entity_count: 1,
                serial_number: Some("private-serial".into()),
                unit_id: [1, 2, 3, 4],
                transports: DeviceTransports::default(),
                model_ids: [0xb034, 0, 0],
                extended_model_id: 2,
            }),
            codename: None,
            kind: DeviceKind::Mouse,
            capabilities: Capabilities::default(),
            light_capabilities: None,
            driver_id: None,
            registry_model_id: None,
        },
    );
    let model = config
        .device_identity("receiver:test:slot:1")
        .and_then(|identity| identity.model_info.as_ref())
        .expect("model info");
    assert_eq!(model.serial_number, None);
    assert_eq!(model.unit_id, [0; 4]);
}

#[test]
fn selected_device_roundtrips() {
    let mut cfg = Config::default();
    assert_eq!(cfg.selected_device(), None);
    cfg.set_selected_device(Some("2b042".into()));
    let parsed = write_and_read(&cfg);
    assert_eq!(parsed.selected_device(), Some("2b042"));
}

#[test]
fn per_app_overlay_takes_precedence() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_binding(
        "2b042",
        ButtonId::Forward,
        Binding::Single(Action::BrowserForward),
    );
    cfg.set_per_app_binding(
        "2b042",
        "com.microsoft.VSCode",
        ButtonId::Back,
        Some(Action::Undo),
    );

    // Global: both buttons are browser nav.
    let global = cfg.effective_bindings("2b042", None);
    assert_eq!(
        global.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    assert_eq!(
        global.get(&ButtonId::Forward),
        Some(&Binding::Single(Action::BrowserForward))
    );

    // VSCode: Back overridden (wrapped as Single), Forward inherits.
    let vscode = cfg.effective_bindings("2b042", Some("com.microsoft.VSCode"));
    assert_eq!(
        vscode.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Undo))
    );
    assert_eq!(
        vscode.get(&ButtonId::Forward),
        Some(&Binding::Single(Action::BrowserForward))
    );

    // Unrelated app falls through.
    let other = cfg.effective_bindings("2b042", Some("com.apple.Safari"));
    assert_eq!(
        other.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
}

#[test]
fn per_app_binding_removal_prunes_empty_app() {
    let mut cfg = Config::default();
    cfg.set_per_app_binding(
        "2b042",
        "com.example.App",
        ButtonId::Back,
        Some(Action::Copy),
    );
    cfg.set_per_app_binding("2b042", "com.example.App", ButtonId::Back, None);
    assert!(
        cfg.devices["2b042"].per_app_bindings.is_empty(),
        "removing last override should prune the app entry"
    );
}

#[test]
fn windows_exe_selector_matches_versioned_path() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_per_app_binding(
        "2b042",
        "exe:sharex.exe",
        ButtonId::Back,
        Some(Action::Copy),
    );
    cfg.set_per_app_binding(
        "2b042",
        "exe:sharex.exe",
        ButtonId::Forward,
        Some(Action::Paste),
    );

    let store_path = r"c:\program files\windowsapps\sharex_14.0.0.0_x64__abc\sharex.exe";
    let effective = cfg.effective_bindings("2b042", Some(store_path));
    assert_eq!(
        effective.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
    assert_eq!(
        effective.get(&ButtonId::Forward),
        Some(&Binding::Single(Action::Paste))
    );
    assert!(cfg.has_app_override("2b042", store_path));

    // Forward slash separators still resolve (hand-authored configs).
    let unixish = r"c:/tools/sharex/sharex.exe";
    assert_eq!(
        cfg.effective_bindings("2b042", Some(unixish))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );

    // Extension match is case-insensitive; selector key is lower-cased.
    let mixed = r"C:\Tools\ShareX\ShareX.EXE";
    assert_eq!(
        cfg.effective_bindings("2b042", Some(mixed))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
}

#[test]
fn windows_exe_selector_exact_path_takes_precedence() {
    let mut cfg = Config::default();
    let exact = r"c:\program files\windowsapps\sharex_14.0.0.0_x64__abc\sharex.exe";
    cfg.set_per_app_binding(
        "2b042",
        "exe:sharex.exe",
        ButtonId::Back,
        Some(Action::Copy),
    );
    cfg.set_per_app_binding("2b042", exact, ButtonId::Back, Some(Action::Undo));

    assert_eq!(
        cfg.effective_bindings("2b042", Some(exact))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Undo))
    );

    // A different install path still falls back to the stable selector.
    let other = r"c:\program files\windowsapps\sharex_15.0.0.0_x64__abc\sharex.exe";
    assert_eq!(
        cfg.effective_bindings("2b042", Some(other))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
}

#[test]
fn windows_exe_selector_ignores_non_exe_identifiers() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_per_app_binding("2b042", "exe:code.exe", ButtonId::Back, Some(Action::Undo));

    // macOS bundle ids must not be treated as Windows paths.
    assert_eq!(
        cfg.effective_bindings("2b042", Some("com.microsoft.VSCode"))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    assert!(!cfg.has_app_override("2b042", "com.microsoft.VSCode"));
}

#[test]
fn app_settings_default_omits_block() {
    let cfg = Config::default();
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("app_settings"),
        "default app_settings should be omitted: {body}"
    );
}

#[test]
fn app_settings_launch_at_login_roundtrips() {
    let mut cfg = Config::default();
    cfg.app_settings.launch_at_login = false;
    let parsed = write_and_read(&cfg);
    assert!(!parsed.app_settings.launch_at_login);
}

#[test]
fn app_settings_smooth_scroll_is_opt_in_and_roundtrips() {
    let default: Config = toml::from_str("schema_version = 5").expect("parse defaults");
    assert!(!default.app_settings.smooth_scroll);

    let mut cfg = Config::default();
    cfg.app_settings.smooth_scroll = true;
    let parsed = write_and_read(&cfg);
    assert!(parsed.app_settings.smooth_scroll);
}

#[test]
fn app_settings_vertical_scroll_sensitivity_defaults_and_roundtrips() {
    let default: Config = toml::from_str("schema_version = 5").expect("parse defaults");
    assert_eq!(
        default.app_settings.vertical_scroll_sensitivity,
        VerticalScrollSensitivity::DEFAULT
    );

    let mut cfg = Config::default();
    cfg.app_settings.vertical_scroll_sensitivity =
        VerticalScrollSensitivity::try_new(7).expect("valid sensitivity");
    let parsed = write_and_read(&cfg);
    assert_eq!(
        parsed.app_settings.vertical_scroll_sensitivity,
        VerticalScrollSensitivity::try_new(7).expect("valid sensitivity")
    );
}

#[test]
fn app_settings_ui_scale_roundtrips() {
    let mut cfg = Config::default();
    cfg.app_settings.ui_scale = UiScale::ExtraLarge;

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    let parsed = write_and_read(&cfg);

    assert!(body.contains("ui_scale = \"extra_large\""));
    assert_eq!(parsed.app_settings.ui_scale, UiScale::ExtraLarge);
}

#[test]
fn config_without_ui_scale_uses_standard_scale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, "schema_version = 4\n").expect("write v4 config");
    let parsed = Config::load_from_path(&path).expect("v4 config should load");

    assert_eq!(parsed.app_settings.ui_scale, UiScale::Normal);
}

#[test]
fn device_view_mode_roundtrips_and_defaults_to_grid() {
    let mut cfg = Config::default();
    cfg.app_settings.device_view_mode = DeviceViewMode::Carousel;

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    let parsed = write_and_read(&cfg);
    let without_preference: Config =
        toml::from_str("schema_version = 4\n").expect("config predating the view preference loads");

    assert!(body.contains("device_view_mode = \"carousel\""));
    assert_eq!(
        parsed.app_settings.device_view_mode,
        DeviceViewMode::Carousel
    );
    assert_eq!(
        without_preference.app_settings.device_view_mode,
        DeviceViewMode::Grid
    );
}

#[test]
fn asset_source_preference_roundtrips() {
    let mut cfg = Config::default();
    cfg.app_settings.asset_source = AssetSourcePreference::OpenLogi;

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    let parsed = write_and_read(&cfg);

    assert!(body.contains("asset_source = \"openlogi\""));
    assert_eq!(
        parsed.app_settings.asset_source,
        AssetSourcePreference::OpenLogi
    );
}

#[test]
fn config_without_asset_source_keeps_automatic_selection() {
    let parsed: Config = toml::from_str(
        r"
            schema_version = 3
            [app_settings]
            auto_download_assets = false
        ",
    )
    .expect("config predating the asset-source setting loads");

    assert_eq!(
        parsed.app_settings.asset_source,
        AssetSourcePreference::Automatic
    );
}

#[test]
fn cleared_selected_device_omits_field() {
    let mut cfg = Config::default();
    cfg.set_selected_device(Some("2b042".into()));
    cfg.set_selected_device(None);
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("selected_device"),
        "cleared selection should not appear: {body}"
    );
}

#[test]
fn empty_device_block_is_skipped_in_output() {
    // Inserting then clearing should not leave a [devices."x"] header
    // with no bindings under it (skip_serializing_if on bindings).
    let mut cfg = Config::default();
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.devices
        .get_mut("2b042")
        .expect("entry")
        .bindings
        .clear();
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        !body.contains("Back"),
        "cleared bindings should not appear: {body}"
    );
}

#[test]
fn migrates_v1_button_and_gesture_bindings() {
    // A pre-v2 file: split button_bindings + a flat gesture_bindings map.
    let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
Back = \"BrowserBack\"

[devices.2b042.gesture_bindings]
Up = \"Copy\"
Click = \"Paste\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v1).expect("write");

    // v1 still loads (version <= current) and folds into the merged map.
    let cfg = Config::load_from_path(&path).expect("load v1");
    let bindings = cfg.bindings_for("2b042");
    assert_eq!(
        bindings.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    let mut gesture = BTreeMap::new();
    gesture.insert(GestureDirection::Up, Action::Copy);
    gesture.insert(GestureDirection::Click, Action::Paste);
    assert_eq!(
        bindings.get(&ButtonId::GestureButton),
        Some(&Binding::Gesture(gesture))
    );

    // Saving self-heals to the current shape: stamped version + merged table,
    // legacy field names gone.
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(
        body.contains(&format!("schema_version = {SCHEMA_VERSION}")),
        "got: {body}"
    );
    assert!(body.contains("[devices.2b042.bindings]"), "got: {body}");
    assert!(!body.contains("button_bindings"), "got: {body}");
    assert!(!body.contains("gesture_bindings"), "got: {body}");
}

#[test]
fn migrates_pre_v7_thumbwheel_defaults_to_normalised_native_direction() {
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
ThumbwheelScrollDown = \"HorizontalScrollLeft\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let bindings = cfg.bindings_for("unit:6be9d300");
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
}

#[test]
fn migrates_a_pre_v7_per_app_pair_that_was_effectively_default() {
    // The app overrides only one half; the other old default comes from a
    // custom global pair. The migration must materialise both new defaults in
    // the app so it remains native without changing the custom global profile.
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"NextTab\"
ThumbwheelScrollDown = \"HorizontalScrollLeft\"

[devices.\"unit:6be9d300\".per_app_bindings.\"com.example.Editor\"]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let app = cfg.effective_bindings("unit:6be9d300", Some("com.example.Editor"));
    assert_eq!(
        app.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
    assert_eq!(
        app.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
    assert_eq!(
        cfg.bindings_for("unit:6be9d300")
            .get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::NextTab)),
        "the custom global pair must stay literal"
    );
}

#[test]
fn pre_v7_thumbwheel_migration_leaves_a_custom_pair_unchanged() {
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
ThumbwheelScrollDown = \"NextTab\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let bindings = cfg.bindings_for("unit:6be9d300");
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::NextTab))
    );
}

#[test]
fn pre_v7_thumbwheel_migration_preserves_non_single_binding_shapes() {
    let v6 = "\
schema_version = 6

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = { short = \"HorizontalScrollRight\", long = \"NextTab\" }
ThumbwheelScrollDown = \"HorizontalScrollLeft\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v6).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v6");
    let bindings = cfg.bindings_for("unit:6be9d300");
    let Some(Binding::LongPress(up)) = bindings.get(&ButtonId::ThumbwheelScrollUp) else {
        panic!("custom long-press binding must keep its shape");
    };
    assert_eq!(up.short(), &Action::HorizontalScrollRight);
    assert_eq!(up.long(), &Action::NextTab);
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
}

#[test]
fn v7_reversed_thumbwheel_pair_survives_reload() {
    let v7 = "\
schema_version = 7

[devices.\"unit:6be9d300\".bindings]
ThumbwheelScrollUp = \"HorizontalScrollRight\"
ThumbwheelScrollDown = \"HorizontalScrollLeft\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v7).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v7");
    let bindings = cfg.bindings_for("unit:6be9d300");
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollUp),
        Some(&Binding::Single(Action::HorizontalScrollRight))
    );
    assert_eq!(
        bindings.get(&ButtonId::ThumbwheelScrollDown),
        Some(&Binding::Single(Action::HorizontalScrollLeft))
    );
}

#[test]
fn migration_gesture_map_wins_over_legacy_single_gesture_button_entry() {
    // The data-loss guard: when a legacy single button_bindings[GestureButton]
    // entry coexists with a gesture_bindings map (reachable via hand-edited
    // or very old configs), the gesture map must survive — not be shadowed by
    // the single entry. Mirrors the pre-v2 "gesture entries win" rule.
    let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
GestureButton = \"MissionControl\"

[devices.2b042.gesture_bindings]
Up = \"Copy\"
Down = \"Paste\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v1).expect("write");

    let cfg = Config::load_from_path(&path).expect("load v1");
    let mut gesture = BTreeMap::new();
    gesture.insert(GestureDirection::Up, Action::Copy);
    gesture.insert(GestureDirection::Down, Action::Paste);
    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Gesture(gesture)),
        "gesture map must win over the legacy single GestureButton entry"
    );
}

#[test]
fn migration_drops_vestigial_lone_gesture_button_single() {
    // A v1 file with only `button_bindings[GestureButton]` and no
    // `gesture_bindings` (the pre-gesture-picker shape). That entry never
    // dispatched in v1 — the gesture button's plain press routes through the
    // gesture `Click` slot, not the per-button map — so migrating it to a
    // `Binding::Single` would leave an unreachable entry the GUI hides and the
    // runtime ignores. It must be dropped, not shadow the gesture path.
    let v1 = "\
schema_version = 1

[devices.2b042.button_bindings]
GestureButton = \"MissionControl\"
Back = \"BrowserBack\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, v1).expect("write");

    let bindings = Config::load_from_path(&path)
        .expect("load v1")
        .bindings_for("2b042");
    // An ordinary button still migrates to a `Single`...
    assert_eq!(
        bindings.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    // ...but the vestigial gesture-button single is gone, leaving the button
    // to fall back to its canonical default rather than an unreachable entry.
    assert_eq!(bindings.get(&ButtonId::GestureButton), None);
}

#[test]
fn rejects_newer_schema_version_but_accepts_v1() {
    // A future version is rejected loudly; the current and older versions
    // load (older ones migrate through the shim).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, "schema_version = 99\n").expect("write");
    assert_matches!(
        Config::load_from_path(&path).expect_err("v99 should fail"),
        ConfigError::UnsupportedSchemaVersion { found: 99, .. }
    );

    fs::write(&path, "schema_version = 1\n").expect("write");
    assert!(
        Config::load_from_path(&path).is_ok(),
        "v1 should still load"
    );
}

#[test]
fn future_version_is_rejected_before_incompatible_fields_are_parsed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "schema_version = 99\n[app_settings]\nthumbwheel_sensitivity = \"future\"\n",
    )
    .expect("write");
    assert_matches!(
        Config::load_from_path(&path).expect_err("future schema must fail by version"),
        ConfigError::UnsupportedSchemaVersion { found: 99, .. }
    );
}

#[test]
fn schema_version_zero_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("config.toml");
    fs::write(&path, "schema_version = 0\n").expect("write config");

    assert!(matches!(
        Config::load_from_path(&path),
        Err(ConfigError::UnsupportedSchemaVersion { found: 0, .. })
    ));
}

#[test]
fn current_schema_rejects_unknown_and_obsolete_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "schema_version = 6\n[app_settings]\nthumbwheel_sensitivty = 14\n",
    )
    .expect("write typo");
    assert_matches!(
        Config::load_from_path(&path).expect_err("typo must fail"),
        ConfigError::Parse { .. }
    );

    fs::write(
        &path,
        r#"schema_version = 6
[devices.mouse.identity]
display_name = "Mouse"
kind = "mouse"
capabilities = { buttons = true, pointer = true, lighting = false, scroll_inversion = false, typo = true }
"#,
    )
    .expect("write nested typo");
    assert_matches!(
        Config::load_from_path(&path).expect_err("nested typo must fail"),
        ConfigError::Parse { .. }
    );

    fs::write(
        &path,
        "schema_version = 6\n[devices.mouse]\ngesture_owner = \"Off\"\n",
    )
    .expect("write obsolete field");
    assert_matches!(
        Config::load_from_path(&path).expect_err("current-schema legacy field must fail"),
        ConfigError::ObsoleteField { .. }
    );
}

#[test]
fn persisted_numeric_contracts_reject_unsafe_values() {
    for body in [
        "schema_version = 6\n[app_settings]\nthumbwheel_sensitivity = 0\n",
        "schema_version = 6\n[app_settings]\nthumbwheel_sensitivity = 101\n",
        "schema_version = 6\n[app_settings]\nthumbwheel_sensitivity = -2147483648\n",
        "schema_version = 6\n[app_settings]\nvertical_scroll_sensitivity = 0\n",
        "schema_version = 6\n[app_settings]\nvertical_scroll_sensitivity = 101\n",
        "schema_version = 6\n[app_settings]\nvertical_scroll_sensitivity = -2147483648\n",
        "schema_version = 6\n[devices.mouse]\nthumbwheel_sensitivity = -1\n",
        "schema_version = 6\n[devices.mouse]\ndpi = 65536\n",
        "schema_version = 6\n[devices.mouse]\ndpi_presets = [800, 70000]\n",
    ] {
        assert!(toml::from_str::<Config>(body).is_err(), "accepted: {body}");
    }
}

#[test]
fn tracked_save_preserves_comments_and_rejects_concurrent_edits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "# keep this comment\nschema_version = 6\nselected_device = \"one\" # and this one\n",
    )
    .expect("write");
    let (mut config, mut file) = ConfigFile::load_from_path(&path).expect("load tracked");
    config.set_selected_device(Some("two".into()));
    file.save(&config).expect("save tracked");
    let saved = fs::read_to_string(&path).expect("read saved");
    assert!(saved.contains("# keep this comment"));
    assert!(saved.contains("# and this one"));
    assert!(saved.contains("selected_device = \"two\""));

    let external = format!("{saved}# external editor\n");
    fs::write(&path, &external).expect("external edit");
    config.set_selected_device(Some("three".into()));
    assert_matches!(file.save(&config), Err(ConfigError::Conflict { .. }));
    assert_eq!(fs::read_to_string(path).expect("read conflict"), external);
}

#[test]
fn set_gesture_direction_upgrades_single_to_gesture() {
    let mut cfg = Config::default();
    // Start from a Single binding, then bind a swipe direction.
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_gesture_direction("2b042", ButtonId::Back, GestureDirection::Up, Action::Copy);

    match cfg.bindings_for("2b042").get(&ButtonId::Back) {
        Some(Binding::Gesture(map)) => {
            // The prior single action is preserved as the Click entry.
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&Action::BrowserBack)
            );
            assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
        }
        other => panic!("expected Gesture after upgrade, got {other:?}"),
    }
}

#[test]
fn set_gesture_direction_on_fresh_gesture_button_seeds_click() {
    // Binding one direction on a never-configured gesture button must still
    // persist a `Click`, so the click projection is the canonical default
    // rather than `Action::None` (which reads as a no-op press).
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Up,
        Action::Copy,
    );

    match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&crate::binding::default_gesture_binding(
                    GestureDirection::Click
                )),
                "a fresh gesture button must seed a Click from its default"
            );
        }
        other => panic!("expected Gesture, got {other:?}"),
    }
}

#[test]
fn set_gesture_mode_seeds_a_fresh_button_with_full_directions() {
    let mut cfg = Config::default();
    // The dedicated HID++ gesture button gets the full default direction map.
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            for dir in GestureDirection::ALL {
                assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
            }
        }
        other => panic!("expected full default gesture map, got {other:?}"),
    }

    // A fresh OS-hook button also gets all five directions, not just a Click:
    // its native action stays as Click, and the swipe arms are defaults — so
    // the GUI's shown defaults are exactly what the runtime dispatches.
    cfg.set_gesture_mode("2b042", ButtonId::Forward, true);
    match cfg.bindings_for("2b042").get(&ButtonId::Forward) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&default_binding(ButtonId::Forward))
            );
            for dir in [
                GestureDirection::Up,
                GestureDirection::Down,
                GestureDirection::Left,
                GestureDirection::Right,
            ] {
                assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
            }
        }
        other => panic!("expected full gesture map for Forward, got {other:?}"),
    }
    // Both promotions coexist — no exclusivity.
    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert!(cfg.is_gesture_mode("2b042", ButtonId::Forward));
}

#[test]
fn gesture_state_roundtrips_through_shapes_without_the_owner_field() {
    // Since v4 the binding shape is the whole persisted truth: gesture
    // state survives a save/load cycle with no `gesture_owner` scalar in
    // the document.
    let mut cfg = Config::default();
    cfg.set_gesture_mode("2b042", ButtonId::Back, true);
    cfg.set_gesture_mode("4082d", ButtonId::GestureButton, false);

    let parsed = write_and_read(&cfg);
    assert!(parsed.is_gesture_mode("2b042", ButtonId::Back));
    assert!(
        parsed.is_gesture_mode("2b042", ButtonId::GestureButton),
        "the dedicated button's default gesture mode is untouched by Back's promotion"
    );
    assert!(parsed.gesture_mode_buttons("4082d").is_empty());

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("gesture_owner"), "got: {body}");
}

#[test]
fn invalid_gesture_owner_string_is_tolerated_not_fatal() {
    // A hand-edit typo in gesture_owner must NOT fail the whole-document parse
    // (which would revert every device's settings to defaults). It degrades
    // to "infer" while the rest of the device config survives.
    let toml = "\
schema_version = 2

[devices.2b042]
gesture_owner = \"bogus\"

[devices.2b042.bindings]
Back = \"Copy\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg =
        Config::load_from_path(&path).expect("an invalid gesture_owner must not fail the load");
    // The rest of the device config survived...
    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
    // ...and the bad owner degraded to inference (HID++ button default here),
    // so the dedicated button keeps its default gesture mode.
    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
}

// ── Shape-driven gesture mode (the owner lock removed) ──

#[test]
fn gesture_mode_is_per_button_and_not_exclusive() {
    // The owner lock is gone: promoting a second button must not demote the
    // dedicated gesture button's default gesture mode.
    let mut cfg = Config::default();
    cfg.set_gesture_mode("2b042", ButtonId::MiddleClick, true);

    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert!(cfg.is_gesture_mode("2b042", ButtonId::MiddleClick));
    let buttons = cfg.gesture_mode_buttons("2b042");
    assert!(
        buttons.contains(&ButtonId::GestureButton),
        "got: {buttons:?}"
    );
    assert!(buttons.contains(&ButtonId::MiddleClick), "got: {buttons:?}");
}

#[test]
fn set_gesture_mode_on_keeps_click_and_seeds_directions() {
    // Promoting a single-bound button keeps its action as the Click entry
    // and seeds every swipe arm, so the button exposes the full five-way set.
    let mut cfg = Config::default();
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.set_gesture_mode("2b042", ButtonId::Back, true);

    let bindings = cfg.bindings_for("2b042");
    let Some(Binding::Gesture(map)) = bindings.get(&ButtonId::Back) else {
        panic!(
            "expected a gesture binding, got {:?}",
            bindings.get(&ButtonId::Back)
        );
    };
    assert_eq!(map.get(&GestureDirection::Click), Some(&Action::Copy));
    for dir in [
        GestureDirection::Up,
        GestureDirection::Down,
        GestureDirection::Left,
        GestureDirection::Right,
    ] {
        assert_eq!(
            map.get(&dir),
            Some(&default_gesture_binding(dir)),
            "unseeded arm {dir:?}"
        );
    }
}

#[test]
fn set_gesture_mode_off_demotes_to_the_click_action() {
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Click,
        Action::Paste,
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);

    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn set_gesture_mode_off_without_click_falls_back_to_the_default() {
    // A sparse hand-edited map with no Click must not demote to a dead
    // Single(None) — the button falls back to its canonical single default.
    let mut cfg = Config::default();
    let mut map = BTreeMap::new();
    map.insert(GestureDirection::Up, Action::Copy);
    cfg.set_binding("2b042", ButtonId::Back, Binding::Gesture(map));
    cfg.set_gesture_mode("2b042", ButtonId::Back, false);

    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
}

#[test]
fn migration_demotes_the_dormant_non_owner_gesture_maps() {
    // An owner-locked config: Middle owns gestures; the dedicated button
    // keeps a dormant map from an earlier reign. Shape-driven load keeps the
    // owner gesturing and flattens the dormant map to its Click, so a stored
    // map's presence always MEANS gesture mode.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"MiddleClick\"

[devices.2b042.bindings]
GestureButton = { Up = \"Copy\", Click = \"Paste\" }
MiddleClick = { Up = \"MissionControl\", Click = \"MiddleClick\" }
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::MiddleClick));
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(Action::Paste)),
        "the dormant map demotes to its Click choice"
    );
    // The legacy field is gone on save — the binding shape is the whole truth.
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("gesture_owner"), "got: {body}");
}

#[test]
fn migration_off_pins_the_dedicated_button_out_of_gesture_mode() {
    // gesture_owner = "Off" with no stored bindings: absence would re-enter
    // default gesture mode under shape rules, so the load pins the dedicated
    // button with an explicit Single at its canonical default (which the
    // capture layer treats as native/undiverted).
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"Off\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert!(cfg.gesture_mode_buttons("2b042").is_empty());
    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(default_binding(ButtonId::GestureButton)))
    );
}

#[test]
fn migration_materializes_a_hidpp_owners_missing_map() {
    // A v3 HID++ owner dispatched the seeded default direction map
    // regardless of its stored shape (the runtime seeded at projection
    // time), so an owner with no stored map must not lose gestures when
    // the file is rewritten to the current schema.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"HapticPanel\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::HapticPanel));
    match cfg.bindings_for("2b042").get(&ButtonId::HapticPanel) {
        Some(Binding::Gesture(map)) => {
            for dir in GestureDirection::ALL {
                assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
            }
        }
        other => panic!("expected the owner's materialized default map, got {other:?}"),
    }
    // The non-owner dedicated button is still pinned off.
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
}

#[test]
fn migration_replaces_a_hidpp_owners_single_with_the_default_map() {
    // A hand-edited v3 file: the owner field says GestureButton but its
    // stored binding is a Single. The v3 runtime still dispatched the full
    // default direction map (seeded at projection), so the load must
    // materialize that map rather than keep the never-dispatched Single.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"GestureButton\"

[devices.2b042.bindings]
GestureButton = \"CycleDpiPresets\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&default_gesture_binding(GestureDirection::Click)),
                "v3 dispatched the seeded default Click, not the stored Single"
            );
        }
        other => panic!("expected the owner's materialized default map, got {other:?}"),
    }
}

#[test]
fn off_then_on_restores_customized_swipe_arms() {
    // Turning gesture mode off must not destroy the user's four customized
    // swipe arms: re-enabling restores the map exactly as it was — the
    // guarantee the owner-lock model gave via dormant maps.
    let mut cfg = Config::default();
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Up,
        Action::Copy,
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));

    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Up),
                Some(&Action::Copy),
                "the customized arm survives an off/on round trip"
            );
        }
        other => panic!("expected the restored gesture map, got {other:?}"),
    }
}

#[test]
fn re_promoting_a_genuine_single_keeps_it_as_click() {
    // A user's deliberate Single that happens to equal the button's
    // canonical default must not be mistaken for the pinned-off marker:
    // promoting keeps their action as the Click arm instead of resetting
    // the whole binding to the canonical default map.
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::GestureButton,
        Binding::Single(default_binding(ButtonId::GestureButton)),
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&default_binding(ButtonId::GestureButton)),
                "the user's explicit action stays as Click"
            );
        }
        other => panic!("expected a gesture binding, got {other:?}"),
    }
}

#[test]
fn disabled_gesture_maps_survive_a_save_load_cycle() {
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Down,
        Action::Paste,
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);

    let mut restored = write_and_read(&cfg);
    restored.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match restored.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(map.get(&GestureDirection::Down), Some(&Action::Paste));
        }
        other => panic!("expected the persisted stash restored, got {other:?}"),
    }
}

#[test]
fn migration_stashes_dormant_maps_for_re_enabling() {
    // The owner-lock model preserved every dormant non-owner map for
    // restore-on-reselection. The migration keeps that promise: the demoted
    // map is stashed, and turning the button back on restores it.
    let toml = "\
schema_version = 3

[devices.2b042]
gesture_owner = \"MiddleClick\"

[devices.2b042.bindings]
GestureButton = { Up = \"Copy\", Click = \"Paste\" }
MiddleClick = { Up = \"MissionControl\", Click = \"MiddleClick\" }
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let mut cfg = Config::load_from_path(&path).expect("load");
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));

    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.bindings_for("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Up),
                Some(&Action::Copy),
                "the dormant map's arms come back on re-enable"
            );
        }
        other => panic!("expected the stashed dormant map restored, got {other:?}"),
    }
}
#[test]
fn migration_infers_the_owner_for_pre_field_configs() {
    // Pre-owner-field file: a gesture-shaped OS-hook button was the owner by
    // inference, silencing the dedicated button. The load preserves exactly
    // that: Back keeps gesturing, the dedicated button is pinned off.
    let toml = "\
schema_version = 2

[devices.2b042.bindings]
Back = { Up = \"Copy\" }
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg = Config::load_from_path(&path).expect("load");
    assert!(cfg.is_gesture_mode("2b042", ButtonId::Back));
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert_eq!(
        cfg.bindings_for("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(default_binding(ButtonId::GestureButton)))
    );
}

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
fn migrating_v4_drops_the_transport_prefix_from_direct_keys() {
    let source = r#"
schema_version = 4
selected_device = "direct:046d:c08d:unit:6be9d300"

[devices."direct:046d:c08d:unit:6be9d300"]
invert_scroll = true

[devices."receiver:82839805:slot:1"]
dpi = 1600
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert!(config.devices.contains_key("unit:6be9d300"));
    assert!(
        !config
            .devices
            .contains_key("direct:046d:c08d:unit:6be9d300")
    );
    assert_eq!(
        config.selected_device.as_deref(),
        Some("unit:6be9d300"),
        "the selection follows the key"
    );
    assert!(
        config.devices["unit:6be9d300"]
            .links
            .contains_key("direct:046d:c08d"),
        "the route it came from is remembered, not discarded"
    );
    assert!(
        config.devices.contains_key("receiver:82839805:slot:1"),
        "receiver entries are left for runtime adoption"
    );
}

#[test]
fn migrating_two_direct_routes_of_one_device_folds_instead_of_dropping_one() {
    // An MX Master 3S reached over USB *and* over Bluetooth-direct has two v4
    // entries whose keys both rename to `unit:6be9d300`. Inserting would let
    // whichever came later in `BTreeMap` order silently delete the other's
    // bindings, DPI and lighting — the one case where phase A of the
    // migration is neither mechanical nor lossless.
    let source = r#"
schema_version = 4

[devices."direct:046d:b034:unit:6be9d300"]
dpi = 1600
invert_scroll = true

[devices."direct:046d:b034:unit:6be9d300".bindings]
Back = "BrowserBack"

[devices."direct:046d:c08d:unit:6be9d300"]
dpi = 800

[devices."direct:046d:c08d:unit:6be9d300".bindings]
Forward = "BrowserForward"
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert_eq!(config.devices.len(), 1, "one device, one entry");
    let device = &config.devices["unit:6be9d300"];
    assert_eq!(
        device.bindings.len(),
        2,
        "both routes' bindings survive: {:?}",
        device.bindings
    );
    assert_eq!(
        device.dpi,
        Some(Dpi::new(1600)),
        "one value stays canonical"
    );
    assert_eq!(
        device.links["direct:046d:c08d"].overrides.dpi,
        Some(Dpi::new(800)),
        "the other survives as an override on the route it was set for"
    );
    assert!(
        device.links.contains_key("direct:046d:b034"),
        "both routes are indexed: {:?}",
        device.links
    );
}

#[test]
fn loading_hand_edited_duplicate_routes_keeps_the_first_device_key() {
    let source = r#"
schema_version = 6
selected_device = "unit:ffffffff"

[devices.keyboard]
host_switch_targets = ["unit:ffffffff"]

[devices."unit:11111111".links."receiver:82839805:slot:1".overrides]
dpi = 800

[devices."unit:ffffffff".links."receiver:82839805:slot:1".overrides]
dpi = 1600
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write hand-edited config");

    let config = Config::load_from_path(&path).expect("load");

    assert_eq!(
        config.devices["unit:11111111"].links["receiver:82839805:slot:1"]
            .overrides
            .dpi,
        Some(Dpi::new(800)),
        "the lexicographically first device key keeps its complete link"
    );
    assert!(
        !config.devices["unit:ffffffff"]
            .links
            .contains_key("receiver:82839805:slot:1"),
        "the later duplicate no longer indexes the route"
    );
    assert_eq!(config.selected_device.as_deref(), Some("unit:ffffffff"));
    assert_eq!(
        config.devices["keyboard"].host_switch_targets,
        vec!["unit:ffffffff".to_string()],
        "repairing an index does not rename the referenced device entry"
    );
}

#[test]
fn loading_v4_repairs_duplicate_routes_created_by_key_migration() {
    let source = r#"
schema_version = 4
selected_device = "direct:046d:c08d:unit:11111111"

[devices.keyboard]
host_switch_targets = ["direct:046d:c08d:unit:11111111"]

[devices."direct:046d:c08d:unit:11111111"]
dpi = 1600

[devices."unit:ffffffff".links."direct:046d:c08d".overrides]
dpi = 800
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write v4 config");

    let config = Config::load_from_path(&path).expect("load and migrate");

    assert!(
        config.devices["unit:11111111"]
            .links
            .contains_key("direct:046d:c08d"),
        "the first post-migration device key owns the route"
    );
    assert!(
        !config.devices["unit:ffffffff"]
            .links
            .contains_key("direct:046d:c08d"),
        "the pre-existing duplicate is removed"
    );
    assert_eq!(
        config.selected_device.as_deref(),
        Some("unit:11111111"),
        "selection follows the migrated device key"
    );
    assert_eq!(
        config.devices["keyboard"].host_switch_targets,
        vec!["unit:11111111".to_string()],
        "host-switch references follow the migrated device key"
    );
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
fn a_failed_backup_write_leaves_the_migration_backup_still_owed() {
    // The pre-migration file is the user's only recovery path from a
    // key-rewriting migration. If the backup write fails — full disk,
    // read-only directory — and the debt is cleared anyway, the next save
    // that *does* succeed overwrites the v4 file with migrated content and no copy
    // ever exists.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    // Appended to the full file name, not substituted for its extension:
    // `config.toml` + `.v4.bak`, never `config.v4.bak`.
    let backup = dir.path().join("config.toml.v4.bak");
    let original =
        b"schema_version = 4\n\n[devices.\"direct:046d:c08d:unit:6be9d300\"]\ninvert_scroll = true\n";
    fs::write(&path, original).expect("write v4 config");
    // A directory where the backup file belongs: the write fails, and
    // nothing about the config file itself is wrong.
    fs::create_dir(&backup).expect("occupy the backup path");

    let (config, mut file) = ConfigFile::load_from_path(&path).expect("load v4 config");
    let error = file.save(&config).expect_err("the backup write must fail");
    assert_matches!(error, ConfigError::Write { .. });
    assert_eq!(
        fs::read(&path).expect("read config"),
        original,
        "a failed save leaves the v4 file in place"
    );

    fs::remove_dir(&backup).expect("free the backup path");
    file.save(&config).expect("save once the path is writable");
    assert_eq!(
        fs::read(&backup).expect("read migration backup"),
        original,
        "the retried save still has the pre-migration source to write"
    );
}

#[test]
fn migrating_rewrites_host_switch_targets() {
    let source = r#"
schema_version = 4

[devices."receiver:82839805:slot:2"]
host_switch_targets = ["direct:046d:c08d:unit:6be9d300"]
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();
    assert_eq!(
        config.devices["receiver:82839805:slot:2"].host_switch_targets,
        vec!["unit:6be9d300".to_string()],
    );
}

#[test]
fn a_v4_file_with_no_direct_entries_is_untouched() {
    let source = "schema_version = 4\n\n[devices.\"receiver:82839805:slot:1\"]\ndpi = 1600\n";
    let mut config: Config = toml::from_str(source).expect("parses");
    let before = config.devices.clone();
    config.migrate_transport_scoped_keys();
    assert_eq!(config.devices.len(), before.len());
    assert!(config.devices.contains_key("receiver:82839805:slot:1"));
}

#[test]
fn migrating_v4_drops_the_transport_prefix_from_serial_keyed_direct_keys() {
    // The identity fragment is not always `unit:<hex>` — a device that
    // reports a serial keys as `serial:<s>` instead, and the brief names
    // both halves of the mapping equally.
    let source = r#"
schema_version = 4

[devices."direct:046d:c08d:serial:abc123"]
invert_scroll = true
"#;
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert!(config.devices.contains_key("serial:abc123"));
    assert!(
        !config
            .devices
            .contains_key("direct:046d:c08d:serial:abc123")
    );
    assert!(
        config.devices["serial:abc123"]
            .links
            .contains_key("direct:046d:c08d"),
        "the route it came from is remembered, not discarded"
    );
}

#[test]
fn migrating_leaves_a_direct_key_with_no_physical_identity_untouched() {
    // An all-zero unit id is not a physical identity (see
    // `PhysicalDeviceKey::parse`), so this key must not be rewritten — doing
    // so would collide every never-identified direct device onto one
    // `unit:00000000` entry.
    let source = "schema_version = 4\n\n[devices.\"direct:046d:c08d:unit:00000000\"]\ninvert_scroll = true\n";
    let mut config: Config = toml::from_str(source).expect("parses");
    config.migrate_transport_scoped_keys();

    assert!(
        config
            .devices
            .contains_key("direct:046d:c08d:unit:00000000"),
        "a non-physical identity is left keyed exactly as it was loaded"
    );
    assert!(!config.devices.contains_key("unit:00000000"));
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
