//! Keyboard RGB and standalone light settings, including colour parsing and its legacy form.

use super::*;

#[test]
fn lighting_roundtrips_per_device() {
    let mut cfg = Config::default();
    cfg.set_lighting(
        "g513",
        Lighting {
            enabled: true,
            color: "00aabb".parse().expect("valid hex"),
            brightness: 75,
            keys: BTreeMap::new(),
            effect: LightingEffect::Static,
            period_ms: None,
        },
    );
    let restored = write_and_read(&cfg);
    assert_eq!(
        restored.lighting("g513"),
        Some(Lighting {
            enabled: true,
            color: "00aabb".parse().expect("valid hex"),
            brightness: 75,
            keys: BTreeMap::new(),
            effect: LightingEffect::Static,
            period_ms: None,
        })
    );
    assert_eq!(restored.lighting("absent"), None);
}

#[test]
fn per_key_colors_roundtrip_by_key_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        r##"
            schema_version = 3
            [devices.g515.lighting]
            color = "0040ff"
            keys = { esc = "ff0000", LShift = "#00ff00", "1" = "ffffff" }
        "##,
    )
    .expect("write config");

    let cfg = Config::load_from_path(&path).expect("load per-key colors");
    let keys = cfg.lighting("g515").expect("lighting").keys;
    let key = |name: &str| name.parse::<LightingKey>().expect("a known key");
    assert_eq!(
        keys,
        BTreeMap::from([
            (key("Esc"), crate::color::Rgb::new(0xff, 0, 0)),
            (key("LShift"), crate::color::Rgb::new(0, 0xff, 0)),
            (key("1"), crate::color::Rgb::WHITE),
        ])
    );

    // Saved back under the canonical names, colors without `#`.
    cfg.save_to_path(&path).expect("save");
    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(saved.contains("Esc = \"ff0000\""), "{saved}");
    assert!(saved.contains("LShift = \"00ff00\""), "{saved}");
    assert_eq!(
        write_and_read(&cfg).lighting("g515").map(|l| l.keys),
        Some(keys)
    );
}

#[test]
fn a_solid_color_saves_no_key_map() {
    let mut cfg = Config::default();
    cfg.set_lighting("g513", Lighting::default());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    cfg.save_to_path(&path).expect("save");
    let saved = fs::read_to_string(path).expect("read saved config");
    assert!(saved.contains("[devices.g513.lighting]"), "{saved}");
    assert!(!saved.contains("keys"), "{saved}");
}

#[test]
fn an_unknown_key_name_is_rejected() {
    let error = toml::from_str::<Config>(
        r#"
            schema_version = 3
            [devices.g515.lighting]
            keys = { Hyper = "ff0000" }
        "#,
    )
    .expect_err("an unknown key must fail");
    assert!(error.to_string().contains("unknown key name"), "{error}");

    let error = toml::from_str::<Config>(
        r#"
            schema_version = 3
            [devices.g515.lighting]
            keys = { Esc = "red" }
        "#,
    )
    .expect_err("an invalid key color must fail");
    assert!(error.to_string().contains("key Esc"), "{error}");
}

#[test]
fn two_spellings_of_one_key_are_rejected() {
    // TOML keeps `Esc` and `esc` apart; key names ignore case, so one would
    // silently replace the other and vanish on the next save.
    let error = toml::from_str::<Config>(
        r#"
            schema_version = 3
            [devices.g515.lighting]
            keys = { Esc = "ff0000", esc = "00ff00" }
        "#,
    )
    .expect_err("two spellings of one key must fail");
    assert!(error.to_string().contains("name the same key"), "{error}");
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
fn an_effect_and_its_pace_roundtrip_and_a_static_one_stays_out_of_the_file() {
    let mut cfg = Config::default();
    let breathing = Lighting {
        effect: LightingEffect::Breathing,
        period_ms: Some(2500),
        ..Lighting::default()
    };
    cfg.set_lighting("g515", breathing.clone());
    cfg.set_lighting("plain", Lighting::default());
    let text = toml::to_string(&cfg).expect("config serializes");
    assert!(text.contains("effect = \"breathing\""), "{text}");
    assert!(text.contains("period_ms = 2500"), "{text}");
    assert!(!text.contains("effect = \"static\""), "{text}");
    let restored = write_and_read(&cfg);
    assert_eq!(restored.lighting("g515"), Some(breathing));
    assert_eq!(restored.lighting("plain"), Some(Lighting::default()));
}
