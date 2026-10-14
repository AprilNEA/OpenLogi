//! Config tests: the shared fixtures, and one module per area.

use std::{assert_matches, fs};

use super::*;
use crate::binding::{G502_LIGHTSPEED, default_binding, default_gesture_binding};
use crate::hid::{Dpi, ReportRate, SmartShiftAutoDisengage, SmartShiftThreshold, TunableTorque};

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
