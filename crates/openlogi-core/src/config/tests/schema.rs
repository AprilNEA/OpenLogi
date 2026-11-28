//! Schema versions and the strictness of the current one.

use std::collections::BTreeSet;

use super::*;

#[test]
fn rejects_next_schema_version_but_accepts_v1() {
    // A future version is rejected loudly; the current and older versions
    // load (older ones migrate through the shim).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let future = SCHEMA_VERSION + 1;
    fs::write(&path, format!("schema_version = {future}\n")).expect("write");
    let error = Config::load_from_path(&path).expect_err("next schema should fail");
    assert_matches!(
        error,
        ConfigError::UnsupportedSchemaVersion { found, .. } if found == future
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
fn v9_without_disabled_keys_loads_and_a_save_stamps_the_current_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "schema_version = 9\n[devices.keyboard]\nfn_lock = true\n",
    )
    .expect("write v9 config");

    let (mut config, mut file) = ConfigFile::load_from_path(&path).expect("load v9 config");
    assert_eq!(config.schema_version, SCHEMA_VERSION);
    assert_eq!(config.disabled_keys("keyboard"), None);

    let disabled_keys = BTreeSet::from([DisableKey::CapsLock, DisableKey::WindowsCommand]);
    config.set_disabled_keys("keyboard", disabled_keys.clone());
    file.save(&config).expect("save v10 config");

    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(
        saved.starts_with(&format!("schema_version = {SCHEMA_VERSION}\n")),
        "saved header: {saved}"
    );
    let reloaded = Config::load_from_path(&path).expect("reload v10 config");
    assert_eq!(reloaded.disabled_keys("keyboard"), Some(&disabled_keys));
}

/// The keypad's LCD settings are optional additions: a file that never sets
/// one must stay readable by the last released build, so saving it keeps the
/// v11 header, and a v11 file that does set them still loads them. The header
/// is spelled out rather than read from `SCHEMA_VERSION`, so a bump has to
/// change this test on purpose.
#[test]
fn unused_console_settings_preserve_schema_compatibility() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "schema_version = 11\n[devices.\"unit:01020304\".bindings]\ngridTopLeft = \"Copy\"\n",
    )
    .expect("write v11 keypad config");
    let (config, mut file) = ConfigFile::load_from_path(&path).expect("load v11 keypad config");
    file.save(&config).expect("save keypad config");
    let saved = fs::read_to_string(&path).expect("read saved config");
    assert!(
        saved.starts_with("schema_version = 11\n"),
        "an unused console setting raised the schema: {saved}"
    );
    for field in ["key_display_brightness", "key_faces", "live_keys"] {
        assert!(!saved.contains(field), "unset `{field}` was saved: {saved}");
    }

    fs::write(
        &path,
        "schema_version = 11\n\
         [devices.\"unit:01020304\"]\n\
         key_display_brightness = 60\n\
         [devices.\"unit:01020304\".key_faces.gridTopLeft]\n\
         label = \"Build\"\n",
    )
    .expect("write v11 config with console settings");
    let config = Config::load_from_path(&path).expect("a v11 file holds console settings");
    assert_eq!(
        config.key_display_brightness("unit:01020304"),
        Some(crate::config::KeyDisplayBrightness::try_new(60).expect("valid brightness"))
    );
    let faces = &config.devices["unit:01020304"].key_faces;
    assert_eq!(
        faces.values().next().and_then(|face| face.label.as_deref()),
        Some("Build")
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
