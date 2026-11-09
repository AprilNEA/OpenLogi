//! Settings released builds saved for a model-only direct device under the
//! all-zero unit id, and how loading moves them to the device's model key.

use super::*;
use crate::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};

/// A real file from a released build: a G402's DPI presets, DPI and identity,
/// keyed `direct:046d:c07e:unit:00000000`.
const RELEASED_G402: &str = include_str!("fixtures/released-model-only-direct-device.toml");
const LEGACY_KEY: &str = "direct:046d:c07e:unit:00000000";
const MODEL_KEY: &str = "direct:046d:c07e";

fn load(source: &str) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write config");
    Config::load_from_path(&path).expect("load")
}

/// `source` with every `from` turned into `to`. An edit that finds nothing
/// still leaves a valid file, so it fails here rather than as a confusing
/// assertion on the settings it should have changed.
///
/// `include_str!` keeps the line endings of the checkout, which are CRLF on a
/// Windows runner: they are normalised first so a `from` spanning a line end
/// matches on every platform.
fn edited(source: &str, from: &str, to: &str) -> String {
    let source = source.replace("\r\n", "\n");
    assert!(
        source.contains(from),
        "{from:?} is not in the text to edit:\n{source}"
    );
    source.replace(from, to)
}

fn g402() -> DeviceStableId {
    DeviceStableId::Direct {
        vendor_id: 0x046d,
        product_id: 0xc07e,
        identity: Some(DeviceIdentity::Model),
    }
}

#[test]
fn an_edit_matches_a_line_end_in_a_crlf_text() {
    assert_eq!(
        edited("a = 1\r\nb = 2\r\n", "a = 1\n", "a = 3\n"),
        "a = 3\nb = 2\n"
    );
}

#[test]
#[should_panic(expected = "is not in the text to edit")]
fn an_edit_that_finds_nothing_fails_the_test() {
    edited("a = 1\n", "b = 2", "b = 3");
}

#[test]
fn released_settings_under_the_zero_unit_key_move_to_the_model_key() {
    let config = load(RELEASED_G402);

    assert!(
        !config.devices.contains_key(LEGACY_KEY),
        "the released entry is consumed once folded"
    );
    let device = &config.devices[MODEL_KEY];
    assert_eq!(
        device.dpi_presets,
        [1512, 840, 1008].map(Dpi::new).to_vec(),
        "the presets the released build saved survive the update"
    );
    assert_eq!(device.dpi, Some(Dpi::new(1008)));
    assert_eq!(
        device
            .identity
            .as_ref()
            .map(|identity| identity.display_name.as_str()),
        Some("Gaming Mouse G402")
    );
    assert!(device.links.contains_key(MODEL_KEY));
    assert!(
        device.links[MODEL_KEY].overrides == LinkOverrides::default(),
        "a value set on the model's own route is the device default, not an override"
    );
    assert_eq!(
        config
            .resolve_device_key(&g402(), Some(&DeviceIdentity::Model))
            .map(PhysicalDeviceKey::into_string),
        Some(MODEL_KEY.to_string()),
        "the agent and the GUI resolve the G402 to the entry holding its settings"
    );
}

#[test]
fn the_move_is_saved_and_not_redone_on_the_next_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, RELEASED_G402).expect("write config");

    let config = Config::load_from_path(&path).expect("load");
    config.save_to_path(&path).expect("save");
    let saved = fs::read_to_string(&path).expect("read saved config");

    assert!(!saved.contains(LEGACY_KEY), "{saved}");
    let reloaded = Config::load_from_path(&path).expect("reload");
    assert_eq!(reloaded.devices, config.devices);
}

#[test]
fn an_existing_model_entry_keeps_its_values_and_takes_only_what_it_lacks() {
    // Both keys hold settings only when the file went through a released
    // build again after this one, or was edited by hand. Nothing says which
    // is newer, so the key this version reads and writes keeps its values;
    // the released entry fills what it leaves unset.
    let source = format!(
        "{RELEASED_G402}\n\
         [devices.\"{MODEL_KEY}\"]\n\
         dpi_presets = [800, 1596]\n\
         dpi = 1596\n"
    );
    let config = load(&source);

    assert!(!config.devices.contains_key(LEGACY_KEY));
    let device = &config.devices[MODEL_KEY];
    assert_eq!(device.dpi_presets, [800, 1596].map(Dpi::new).to_vec());
    assert_eq!(device.dpi, Some(Dpi::new(1596)));
    assert_eq!(
        device.links[MODEL_KEY].overrides.dpi, None,
        "the released DPI must not come back as an override on the only route"
    );
    assert_eq!(
        device
            .identity
            .as_ref()
            .map(|identity| identity.display_name.as_str()),
        Some("Gaming Mouse G402"),
        "the identity the model entry lacked is taken from the released one"
    );
}

#[test]
fn a_released_page_count_survives_a_model_entry_that_never_added_pages() {
    let source = format!(
        "{}\n\
         [devices.\"{MODEL_KEY}\"]\n\
         dpi = 1596\n",
        edited(
            RELEASED_G402,
            "dpi = 1008\n",
            "dpi = 1008\nkeypad_pages = 3\n"
        )
    );
    let config = load(&source);

    assert!(!config.devices.contains_key(LEGACY_KEY));
    assert_eq!(config.devices[MODEL_KEY].keypad_pages.number(), 3);
}

#[test]
fn a_zero_unit_entry_on_a_route_owned_by_a_reported_unit_is_left_alone() {
    // This model reports a unit id: the zero-unit entry is a half-read probe
    // of that device, not a model-only device.
    let source = format!(
        "schema_version = 11\n\
         \n\
         [devices.\"unit:6be9d300\"]\n\
         dpi = 1600\n\
         [devices.\"unit:6be9d300\".links.\"direct:046d:c08d\"]\n\
         \n\
         {}",
        edited(
            &edited(RELEASED_G402, "schema_version = 11", ""),
            "046d:c07e",
            "046d:c08d"
        )
    );
    let config = load(&source);

    assert!(
        config
            .devices
            .contains_key("direct:046d:c08d:unit:00000000")
    );
    assert!(!config.devices.contains_key("direct:046d:c08d"));
}

#[test]
fn a_zero_unit_entry_with_no_recorded_identity_is_left_alone() {
    // Nothing says the device answered DeviceInformation: it may be a probe
    // that never read a unit id the device has.
    let config = load(&format!(
        "schema_version = 11\n\n[devices.\"{LEGACY_KEY}\"]\ndpi = 1008\n"
    ));

    assert!(config.devices.contains_key(LEGACY_KEY));
    assert!(!config.devices.contains_key(MODEL_KEY));
}
