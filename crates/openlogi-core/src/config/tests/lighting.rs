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
