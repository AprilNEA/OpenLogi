//! Keyboard RGB and standalone light settings, including colour parsing and its legacy form.

use super::*;

#[test]
fn rgb_device_edit_reads_effective_link_state_but_refuses_a_shadowed_write() {
    use crate::device_order::{DeviceIdentity, DeviceStableId};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let stable = DeviceStableId::Bolt {
        receiver_uid: "rgb-test".into(),
        slot: 1,
    };
    let identity = DeviceIdentity::Unit([1, 2, 3, 4]);
    let base = Lighting {
        enabled: true,
        color: crate::color::Rgb::WHITE,
        brightness: 50,
    };
    let override_value = Lighting {
        enabled: false,
        brightness: 23,
        ..base.clone()
    };
    let mut config = Config::default();
    config.set_lighting("unit:01020304", base.clone());
    config
        .devices
        .get_mut("unit:01020304")
        .unwrap()
        .links
        .insert(
            stable.route_key(),
            LinkConfig {
                overrides: LinkOverrides {
                    lighting: Some(override_value.clone()),
                    ..LinkOverrides::default()
                },
                ..LinkConfig::default()
            },
        );
    config.save_to_path(&path).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    let mut edit = DeviceEdit::load(&path, &stable, Some(&identity)).unwrap();
    assert_eq!(edit.lighting(), Some(&override_value));
    assert!(matches!(
        edit.set_lighting(base),
        Err(DeviceEditError::LinkOverride)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn rgb_device_edit_conflict_keeps_the_other_writers_settings() {
    use crate::device_order::{DeviceIdentity, DeviceStableId};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    Config::default().save_to_path(&path).unwrap();
    let stable = DeviceStableId::Bolt {
        receiver_uid: "rgb-test".into(),
        slot: 1,
    };
    let identity = DeviceIdentity::Unit([1, 2, 3, 4]);
    let mut edit = DeviceEdit::load(&path, &stable, Some(&identity)).unwrap();
    edit.set_lighting(Lighting::default()).unwrap();
    let mut external = Config::load_from_path(&path).unwrap();
    external.set_dpi("unit:87654321", 1200.into());
    external.save_to_path(&path).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    assert!(matches!(edit.commit(), Err(ConfigError::Conflict { .. })));
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
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

/// A light that takes a command outside the settings app keeps it in the
/// settings saved for it. A light with none stays without: the agent writes
/// nothing to it when it reconnects, so saving defaults alongside the one
/// command would change the controls the user left alone.
#[test]
fn a_light_command_is_saved_only_in_settings_the_light_already_has() {
    use crate::device_order::{DeviceIdentity as Identity, DeviceStableId};
    use crate::hid::{DIRECT_DEVICE_INDEX, DeviceRoute, LightCommand};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let route = DeviceRoute::RawHid {
        vendor_id: 0x046d,
        product_id: 0xc900,
        usage_page: 0xff43,
        usage_id: 0x0202,
        identity: "serial:glow".to_owned(),
    };
    let stable =
        DeviceStableId::from_parts(Some(&route), DIRECT_DEVICE_INDEX, Some("glow"), [0; 4]);
    let identity = Identity::from_parts(Some("glow"), [0; 4]);
    let key = Config::default()
        .resolve_device_key(&stable, Some(&identity))
        .expect("a serial names the light");
    let dim = |path: &std::path::Path| {
        let mut edit = DeviceEdit::load(path, &stable, Some(&identity)).expect("load");
        edit.set(DeviceSetting::Light(LightCommand::BrightnessPercent(40)))
            .expect("a light command is never refused");
        edit.commit().expect("save");
        Config::load_from_path(path)
            .expect("reload")
            .light(key.as_str())
    };

    Config::default().save_to_path(&path).expect("save");
    assert_eq!(dim(&path), None);

    let mut config = Config::default();
    config.set_light(key.as_str(), LightSettings::new(false, 80, Some(4000)));
    config.save_to_path(&path).expect("save");
    assert_eq!(dim(&path), Some(LightSettings::new(false, 40, Some(4000))));
}
