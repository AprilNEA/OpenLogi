//! Config tests: the shared fixtures, and one module per area.

use std::{assert_matches, fs};

use super::*;
use crate::binding::{default_binding, default_gesture_binding};
use crate::hid::{Dpi, SmartShiftAutoDisengage, SmartShiftThreshold, TunableTorque};

mod app_settings;
mod device_settings;
mod files;
mod gestures;
mod identity;
mod keyboard;
mod lighting;
mod links;
mod migrations;
mod per_app;
mod schema;

fn write_and_read(config: &Config) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    config.save_to_path(&path).expect("save");
    Config::load_from_path(&path).expect("load")
}

#[test]
fn zoom_sensitivity_prefers_the_device_override_over_the_app_default() {
    let mut config = Config::default();
    config.app_settings.zoom_sensitivity = ThumbwheelSensitivity::from_rounded(20.0);

    assert_eq!(
        config.zoom_sensitivity("mouse-a"),
        ThumbwheelSensitivity::from_rounded(20.0),
        "a device with no override follows the app-wide value"
    );

    config.set_device_zoom_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MAX));
    assert_eq!(
        config.zoom_sensitivity("mouse-a"),
        ThumbwheelSensitivity::MAX
    );
    assert_eq!(
        config.zoom_sensitivity("mouse-b"),
        ThumbwheelSensitivity::from_rounded(20.0),
        "one device's override must not leak to another"
    );

    config.set_device_zoom_sensitivity("mouse-a", None);
    assert_eq!(
        config.zoom_sensitivity("mouse-a"),
        ThumbwheelSensitivity::from_rounded(20.0),
        "clearing the override falls back to the app-wide value"
    );
}

#[test]
fn zoom_sensitivity_is_independent_of_thumbwheel_sensitivity() {
    // The whole point of the separate setting: tuning zoom must not move
    // scroll speed, and vice versa.
    let mut config = Config::default();
    config.set_device_zoom_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MAX));

    assert_eq!(
        config.zoom_sensitivity("mouse-a"),
        ThumbwheelSensitivity::MAX
    );
    assert_eq!(
        config.thumbwheel_sensitivity("mouse-a"),
        ThumbwheelSensitivity::DEFAULT,
        "raising zoom must leave scroll sensitivity alone"
    );

    config.set_device_thumbwheel_sensitivity("mouse-a", Some(ThumbwheelSensitivity::MIN));
    assert_eq!(
        config.zoom_sensitivity("mouse-a"),
        ThumbwheelSensitivity::MAX,
        "lowering scroll must leave zoom alone"
    );
}

#[test]
fn config_without_a_zoom_sensitivity_field_loads_at_the_default() {
    // The field is additive: an existing config.toml predates it and must load
    // unchanged rather than failing or shifting the user's zoom speed.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        format!("schema_version = {SCHEMA_VERSION}\n[app_settings]\nthumbwheel_sensitivity = 30\n"),
    )
    .expect("write config");

    let config = Config::load_from_path(&path).expect("load");
    assert_eq!(
        config.app_settings.thumbwheel_sensitivity,
        ThumbwheelSensitivity::from_rounded(30.0)
    );
    assert_eq!(
        config.app_settings.zoom_sensitivity,
        ThumbwheelSensitivity::DEFAULT,
        "a missing zoom sensitivity defaults instead of failing the load"
    );
}
