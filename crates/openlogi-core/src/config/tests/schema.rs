//! Schema versions and the strictness of the current one.

use super::*;

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
