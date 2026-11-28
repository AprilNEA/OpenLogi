//! The `[app]` block: defaults, opt-ins, scale, gallery layout and asset source.

use super::*;
use crate::update_feed::UpdateChannel;

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
fn software_smoothing_is_opt_in_for_each_application() {
    let mut cfg = Config::default();
    assert!(!cfg.smooth_scroll_for(None));
    assert!(!cfg.smooth_scroll_for(Some("com.example.Editor")));
    cfg.set_app_smooth_scroll("com.example.Editor", true);
    assert!(cfg.smooth_scroll_for(Some("com.example.Editor")));
    assert!(!cfg.smooth_scroll_for(Some("com.example.Browser")));
    assert!(!cfg.smooth_scroll_for(None));
    assert!(!cfg.app_settings.smooth_scroll);
    assert!(cfg.devices.is_empty(), "software profiles are app-wide");

    let mut restored = write_and_read(&cfg);
    assert!(restored.smooth_scroll_for(Some("com.example.Editor")));
    assert!(!restored.smooth_scroll_for(Some("com.example.Browser")));
    restored.remove_app_smooth_scroll("com.example.Editor");
    assert!(!restored.smooth_scroll_for(Some("com.example.Editor")));
    let body = toml::to_string_pretty(&restored).expect("serialize");
    assert!(!body.contains("per_app_smooth_scroll"), "{body}");
}

#[test]
fn software_smoothing_preserves_existing_global_settings() {
    for enabled in [false, true] {
        let mut cfg: Config = toml::from_str(&format!(
            "schema_version = 11\n[app_settings]\nsmooth_scroll = {enabled}\n"
        ))
        .expect("existing config");
        assert_eq!(cfg.smooth_scroll_for(None), enabled);
        assert_eq!(cfg.smooth_scroll_for(Some("Editor")), enabled);
        cfg.set_app_smooth_scroll("Editor", !enabled);
        let mut restored = write_and_read(&cfg);
        assert_eq!(restored.smooth_scroll_for(Some("Editor")), !enabled);
        assert_eq!(restored.smooth_scroll_for(Some("Browser")), enabled);
        restored.remove_app_smooth_scroll("Editor");
        assert_eq!(restored.smooth_scroll_for(Some("Editor")), enabled);
    }
}

#[test]
fn explicit_native_scrolling_survives_default_changes() {
    let mut cfg = Config::default();
    cfg.set_app_smooth_scroll("Editor", false);
    let mut restored = write_and_read(&cfg);
    restored.app_settings.smooth_scroll = true;
    assert!(!restored.smooth_scroll_for(Some("Editor")));
    assert!(restored.smooth_scroll_for(Some("Browser")));
}

#[test]
fn per_app_vertical_sensitivity_roundtrips_alongside_smoothing() {
    let config: Config = toml::from_str(
        r#"schema_version = 13
[app_settings]
smooth_scroll = true
vertical_scroll_sensitivity = 18
[app_settings.per_app_smooth_scroll]
"com.example.game" = false
[app_settings.per_app_vertical_scroll_sensitivity]
"com.example.game" = 14
"#,
    )
    .expect("native-distance game profile loads");
    let restored = write_and_read(&config);
    let game = restored.wheel_scroll_profile_for(Some("com.example.game"));
    assert_eq!(game.app, Some("com.example.game"));
    assert!(!game.smooth_scroll);
    assert_eq!(
        game.vertical_sensitivity,
        VerticalScrollSensitivity::DEFAULT
    );
    for app in [None, Some("com.apple.Safari")] {
        let profile = restored.wheel_scroll_profile_for(app);
        assert_eq!(profile.app, app);
        assert_eq!(profile.overrides, WheelScrollOverrides::default());
        assert!(profile.smooth_scroll);
        assert_eq!(u8::from(profile.vertical_sensitivity), 18);
    }
    let body = toml::to_string_pretty(&restored).expect("serialize the restored profile");
    assert!(body.contains("[app_settings.per_app_vertical_scroll_sensitivity]"));
}

#[test]
fn older_wheel_profiles_inherit_the_saved_global_distance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    for (version, profile) in [
        (11, ""),
        (12, "[app_settings.per_app_smooth_scroll]\nGame = false\n"),
    ] {
        fs::write(&path, format!("schema_version = {version}\n[app_settings]\nsmooth_scroll = true\nvertical_scroll_sensitivity = 18\n{profile}")).expect("write earlier config");
        let (cfg, mut file) = ConfigFile::load_from_path(&path).expect("load earlier config");
        let game = cfg.wheel_scroll_profile_for(Some("Game"));
        assert_eq!(game.app, (version == 12).then_some("Game"));
        assert_eq!(game.smooth_scroll, version == 11);
        assert_eq!(u8::from(game.vertical_sensitivity), 18);
        file.save(&cfg).expect("save new schema");
        let saved = fs::read_to_string(&path).expect("read saved config");
        assert!(saved.starts_with(&format!("schema_version = {SCHEMA_VERSION}\n")));
        assert!(!saved.contains("per_app_vertical_scroll_sensitivity"));
        assert_eq!(
            Config::load_from_path(&path)
                .expect("reload")
                .wheel_scroll_profile_for(Some("Game")),
            game
        );
    }
}

#[test]
fn independent_thumbwheel_sensitivity_survives_a_config_save() {
    let config: Config = toml::from_str(
        r"schema_version = 14
[app_settings]
thumbwheel_sensitivity = 28
[devices.mouse]
thumbwheel_sensitivity = 42
[app_settings.per_app_thumbwheel_sensitivity]
Game = 14
",
    )
    .expect("parse app and device thumbwheel sensitivity");
    let mut restored = write_and_read(&config);
    let game = restored.thumbwheel_profile_for(Some("mouse"), Some("Game"));
    assert_eq!(game.sensitivity, ThumbwheelSensitivity::DEFAULT);
    assert!(game.overridden);
    assert_eq!(game.app, Some("Game"));
    for (device, expected) in [(Some("mouse"), 42), (Some("other"), 28), (None, 28)] {
        let profile = restored.thumbwheel_profile_for(device, Some("Safari"));
        assert_eq!(profile.app, Some("Safari"));
        assert!(!profile.overridden);
        assert_eq!(u8::from(profile.sensitivity), expected);
    }
    assert_eq!(
        restored.wheel_scroll_profile_for(Some("Safari")).app,
        Some("Safari")
    );
    assert_eq!(
        restored.wheel_scroll_profile_for(Some("Safari")).overrides,
        WheelScrollOverrides::default()
    );
    let body = toml::to_string_pretty(&restored).expect("serialize");
    assert!(body.contains("[app_settings.per_app_thumbwheel_sensitivity]"));
    assert!(restored.remove_app_thumbwheel_sensitivity("Game"));
    assert!(!restored.remove_app_thumbwheel_sensitivity("Game"));
    let game = restored.thumbwheel_profile_for(Some("mouse"), Some("Game"));
    assert_eq!(u8::from(game.sensitivity), 42);
    assert_eq!(
        game.app, None,
        "clearing the only wheel rule restores global input lifetime"
    );
}

#[test]
fn older_app_profiles_keep_device_and_global_thumbwheel_sensitivity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    for (version, fields) in [
        (11, ""),
        (12, "[app_settings.per_app_smooth_scroll]\nEditor=true\n"),
        (
            13,
            "[app_settings.per_app_vertical_scroll_sensitivity]\nEditor=7\n",
        ),
    ] {
        fs::write(&path, format!("schema_version = {version}\n[app_settings]\nthumbwheel_sensitivity = 18\n[devices.mouse]\nthumbwheel_sensitivity = 21\n{fields}")).expect("write previous-version config");
        let (config, mut file) = ConfigFile::load_from_path(&path).expect("load previous version");
        for (device, expected) in [(Some("mouse"), 21), (None, 18)] {
            let thumb = config.thumbwheel_profile_for(device, Some("Editor"));
            assert_eq!(thumb.app, (version != 11).then_some("Editor"));
            assert!(!thumb.overridden);
            assert_eq!(u8::from(thumb.sensitivity), expected);
        }
        file.save(&config).expect("save updated schema");
        let saved = fs::read_to_string(&path).expect("read");
        assert!(saved.starts_with(&format!("schema_version = {SCHEMA_VERSION}\n")));
        assert!(!saved.contains("per_app_thumbwheel_sensitivity"));
        let reloaded = Config::load_from_path(&path).expect("reload");
        assert_eq!(
            reloaded.thumbwheel_profile_for(Some("mouse"), Some("Editor")),
            config.thumbwheel_profile_for(Some("mouse"), Some("Editor"))
        );
    }
}

#[test]
fn wheel_profile_edits_keep_each_exact_override_independent() {
    let mut cfg = Config::default();
    let native = VerticalScrollSensitivity::DEFAULT;
    assert!(cfg.set_app_vertical_scroll_sensitivity("Game", native));
    assert!(!cfg.set_app_vertical_scroll_sensitivity("Game", native));
    cfg.app_settings.vertical_scroll_sensitivity =
        VerticalScrollSensitivity::try_new(18).expect("valid");
    assert_eq!(cfg.vertical_scroll_sensitivity_for(Some("Game")), native);
    assert!(cfg.set_app_smooth_scroll("Game", false));
    assert!(!cfg.set_app_smooth_scroll("Game", false));
    cfg.set_app_smooth_scroll("Editor", true);
    cfg.set_app_vertical_scroll_sensitivity("Browser", native);
    assert!(cfg.set_app_thumbwheel_sensitivity("Game", ThumbwheelSensitivity::DEFAULT));
    assert!(!cfg.set_app_thumbwheel_sensitivity("Game", ThumbwheelSensitivity::DEFAULT));
    cfg.set_app_thumbwheel_sensitivity("Mail", ThumbwheelSensitivity::MIN);
    let mut keys = cfg.wheel_scroll_profile_keys().collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["Browser", "Editor", "Game", "Mail"]);

    assert!(cfg.remove_app_vertical_scroll_sensitivity("Game"));
    assert!(!cfg.remove_app_vertical_scroll_sensitivity("Game"));
    assert_eq!(
        u8::from(cfg.vertical_scroll_sensitivity_for(Some("Game"))),
        18
    );
    assert_eq!(cfg.wheel_scroll_profile_for(Some("Game")).app, Some("Game"));
    cfg.set_app_vertical_scroll_sensitivity("Game", native);
    assert!(cfg.remove_wheel_scroll_profile("Game"));
    assert!(!cfg.remove_wheel_scroll_profile("Game"));
    let game = cfg.wheel_scroll_profile_for(Some("Game"));
    assert_eq!(
        game.app,
        Some("Game"),
        "other wheel profiles still scope motion to its destination"
    );
    assert_eq!(game.overrides, WheelScrollOverrides::default());
    assert!(!cfg.thumbwheel_profile_for(None, Some("Game")).overridden);
    assert!(!cfg.wheel_scroll_profile_keys().any(|key| key == "Game"));
    assert!(cfg.wheel_scroll_profile_keys().any(|key| key == "Mail"));
    assert!(cfg.wheel_scroll_profile_keys().any(|key| key == "Browser"));
    assert!(cfg.wheel_scroll_profile_keys().any(|key| key == "Editor"));
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
fn low_battery_notifications_stay_on_unless_turned_off() {
    assert!(Config::default().app_settings.low_battery_notifications);

    let predating: Config = toml::from_str(
        r"
            schema_version = 8
            [app_settings]
            auto_download_assets = false
        ",
    )
    .expect("config predating the low-battery setting loads");
    assert!(predating.app_settings.low_battery_notifications);

    let mut cfg = Config::default();
    cfg.app_settings.low_battery_notifications = false;
    assert!(!write_and_read(&cfg).app_settings.low_battery_notifications);
}

#[test]
fn lock_key_notifications_stay_on_unless_turned_off() {
    assert!(Config::default().app_settings.lock_key_notifications);

    let predating: Config = toml::from_str(
        r"
            schema_version = 8
            [app_settings]
            low_battery_notifications = false
        ",
    )
    .expect("config predating the lock-key setting loads");
    assert!(predating.app_settings.lock_key_notifications);

    let mut cfg = Config::default();
    cfg.app_settings.lock_key_notifications = false;
    assert!(!write_and_read(&cfg).app_settings.lock_key_notifications);
}

#[test]
fn desktop_switches_slide_unless_turned_off() {
    assert!(Config::default().app_settings.animate_desktop_switch);

    let predating: Config = toml::from_str(
        r"
            schema_version = 9
            [app_settings]
            lock_key_notifications = false
        ",
    )
    .expect("config predating the desktop-switch setting loads");
    assert!(predating.app_settings.animate_desktop_switch);

    let mut cfg = Config::default();
    cfg.app_settings.animate_desktop_switch = false;
    assert!(!write_and_read(&cfg).app_settings.animate_desktop_switch);
}

#[test]
fn a_config_without_an_update_channel_writes_back_unchanged() {
    let mut cfg = Config::default();
    cfg.app_settings.check_for_updates = true;
    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("update_channel"), "{body}");

    let parsed: Config = toml::from_str(&body).expect("config predating the channel loads");
    assert_eq!(parsed.app_settings.update_channel, None);
    assert_eq!(toml::to_string_pretty(&parsed).expect("serialize"), body);
}

#[test]
fn a_picked_update_channel_roundtrips() {
    for (channel, spelling) in [
        (UpdateChannel::Stable, "stable"),
        (UpdateChannel::Beta, "beta"),
    ] {
        let mut cfg = Config::default();
        cfg.app_settings.update_channel = Some(channel);
        let body = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            body.contains(&format!("update_channel = \"{spelling}\"")),
            "{body}"
        );
        assert_eq!(
            write_and_read(&cfg).app_settings.update_channel,
            Some(channel)
        );
    }
}

#[test]
fn an_unknown_update_channel_is_rejected() {
    let error = toml::from_str::<Config>(
        r#"
            schema_version = 9
            [app_settings]
            update_channel = "nightly"
        "#,
    )
    .expect_err("an unknown channel must not load as another one");
    assert!(error.to_string().contains("nightly"), "{error}");
}
