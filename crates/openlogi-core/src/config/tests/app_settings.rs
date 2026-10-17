//! The `[app]` block: defaults, opt-ins, scale, gallery layout and asset source.

use super::*;

#[test]
fn mouse_profile_target_defaults_for_old_configs_and_roundtrips() {
    for body in [
        "schema_version = 7\n",
        "schema_version = 7\n[app_settings]\nlaunch_at_login = false\n",
    ] {
        let parsed: Config = toml::from_str(body).expect("config predating mouse profile target");
        assert_eq!(
            parsed.app_settings.mouse_profile_target,
            MouseProfileTarget::Pointer
        );
    }
    assert_eq!(
        AppSettings::default().mouse_profile_target,
        MouseProfileTarget::Pointer
    );

    for (target, spelling) in [
        (MouseProfileTarget::Pointer, "pointer"),
        (MouseProfileTarget::Focused, "focused"),
    ] {
        let mut cfg = Config::default();
        cfg.app_settings.launch_at_login = false;
        cfg.app_settings.mouse_profile_target = target;
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(body.contains(&format!("mouse_profile_target = \"{spelling}\"")));
        assert_eq!(
            write_and_read(&cfg).app_settings.mouse_profile_target,
            target
        );
    }
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
