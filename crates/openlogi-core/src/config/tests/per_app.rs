//! Per-app overlays and the application selectors that pick them.

use super::*;

#[test]
fn per_app_overlay_takes_precedence() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_binding(
        "2b042",
        ButtonId::Forward,
        Binding::Single(Action::BrowserForward),
    );
    cfg.set_per_app_binding(
        "2b042",
        "com.microsoft.VSCode",
        ButtonId::Back,
        Some(Action::Undo),
    );

    // Global: both buttons are browser nav.
    let global = cfg.effective_bindings("2b042", None);
    assert_eq!(
        global.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    assert_eq!(
        global.get(&ButtonId::Forward),
        Some(&Binding::Single(Action::BrowserForward))
    );

    // VSCode: Back overridden (wrapped as Single), Forward inherits.
    let vscode = cfg.effective_bindings("2b042", Some("com.microsoft.VSCode"));
    assert_eq!(
        vscode.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Undo))
    );
    assert_eq!(
        vscode.get(&ButtonId::Forward),
        Some(&Binding::Single(Action::BrowserForward))
    );

    // Unrelated app falls through.
    let other = cfg.effective_bindings("2b042", Some("com.apple.Safari"));
    assert_eq!(
        other.get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
}

#[test]
fn per_app_binding_removal_prunes_empty_app() {
    let mut cfg = Config::default();
    cfg.set_per_app_binding(
        "2b042",
        "com.example.App",
        ButtonId::Back,
        Some(Action::Copy),
    );
    cfg.set_per_app_binding("2b042", "com.example.App", ButtonId::Back, None);
    assert!(
        cfg.devices["2b042"].per_app_bindings.is_empty(),
        "removing last override should prune the app entry"
    );
}

#[test]
fn windows_exe_selector_matches_versioned_path() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_per_app_binding(
        "2b042",
        "exe:sharex.exe",
        ButtonId::Back,
        Some(Action::Copy),
    );
    cfg.set_per_app_binding(
        "2b042",
        "exe:sharex.exe",
        ButtonId::Forward,
        Some(Action::Paste),
    );

    let store_path = r"c:\program files\windowsapps\sharex_14.0.0.0_x64__abc\sharex.exe";
    let effective = cfg.effective_bindings("2b042", Some(store_path));
    assert_eq!(
        effective.get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
    assert_eq!(
        effective.get(&ButtonId::Forward),
        Some(&Binding::Single(Action::Paste))
    );
    assert!(cfg.has_app_override("2b042", store_path));

    // Forward slash separators still resolve (hand-authored configs).
    let unixish = r"c:/tools/sharex/sharex.exe";
    assert_eq!(
        cfg.effective_bindings("2b042", Some(unixish))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );

    // Extension match is case-insensitive; selector key is lower-cased.
    let mixed = r"C:\Tools\ShareX\ShareX.EXE";
    assert_eq!(
        cfg.effective_bindings("2b042", Some(mixed))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
}

#[test]
fn windows_exe_selector_exact_path_takes_precedence() {
    let mut cfg = Config::default();
    let exact = r"c:\program files\windowsapps\sharex_14.0.0.0_x64__abc\sharex.exe";
    cfg.set_per_app_binding(
        "2b042",
        "exe:sharex.exe",
        ButtonId::Back,
        Some(Action::Copy),
    );
    cfg.set_per_app_binding("2b042", exact, ButtonId::Back, Some(Action::Undo));

    assert_eq!(
        cfg.effective_bindings("2b042", Some(exact))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Undo))
    );

    // A different install path still falls back to the stable selector.
    let other = r"c:\program files\windowsapps\sharex_15.0.0.0_x64__abc\sharex.exe";
    assert_eq!(
        cfg.effective_bindings("2b042", Some(other))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
}

#[test]
fn windows_exe_selector_ignores_non_exe_identifiers() {
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_per_app_binding("2b042", "exe:code.exe", ButtonId::Back, Some(Action::Undo));

    // macOS bundle ids must not be treated as Windows paths.
    assert_eq!(
        cfg.effective_bindings("2b042", Some("com.microsoft.VSCode"))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
    assert!(!cfg.has_app_override("2b042", "com.microsoft.VSCode"));
}

/// Load `source` from a file, through every check a real load runs.
fn load(source: &str) -> Result<Config, ConfigError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, source).expect("write config");
    Config::load_from_path(&path)
}

/// `source` with `PLATFORM` replaced by the `[apps.<alias>]` field of the
/// platform the tests run on, so one fixture exercises the live matcher on
/// every CI host.
fn on_this_platform(source: &str) -> String {
    // CI runs these tests on every platform, so the fixture's `[apps]` table
    // must be valid under each field, not only under this host's: a Windows
    // identifier must be lower-case.
    for platform in crate::app::AppPlatform::ALL {
        let field = platform.field();
        let fixture: toml::Table = source
            .replace("PLATFORM", field)
            .parse()
            .expect("fixture must be TOML");
        let apps = fixture
            .get("apps")
            .cloned()
            .expect("fixture defines [apps]");
        if let Err(error) = apps.try_into::<crate::app::AppAliases>() {
            panic!("fixture must be valid under `{field}`: {error}");
        }
    }
    source.replace("PLATFORM", crate::app::AppPlatform::CURRENT.field())
}

#[test]
fn the_documented_alias_profile_applies_on_this_platform() {
    let config = load(include_str!("../../../../../docs/config.example.toml"))
        .expect("documented config must load");
    let (_, chrome) = config
        .apps
        .for_profile_key("app:chrome")
        .expect("the example defines the chrome alias");
    assert_eq!(chrome.display_name(), Some("Google Chrome"));
    let here = chrome
        .identifiers_here()
        .first()
        .expect("the example names Chrome on every platform");
    let device = "receiver:aabbccdd:slot:1";
    assert_eq!(config.app_profile_for(device, here), Some("app:chrome"));
    assert_eq!(
        config
            .effective_bindings(device, Some(here))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::PrevTab))
    );
}

#[test]
fn an_alias_profile_drives_the_buttons_and_the_actions_ring() {
    use crate::binding::{ActionRingEntry, ActionRingSlot, RingAction};
    let config = load(&on_this_platform(
        r#"schema_version = 9
[devices.mouse.bindings]
Back = "BrowserBack"
[devices.mouse.per_app_bindings."app:editor"]
Back = "Undo"
[devices.mouse.action_ring.default.slots]
Top = { action = "Copy" }
[devices.mouse.action_ring.per_app."app:editor".slots]
Top = { action = "Paste" }
[apps.editor]
PLATFORM = ["com.example.editor", "com.example.editorbeta"]
"#,
    ))
    .expect("alias profiles must load");
    let ring = config.action_ring("mouse");
    let paste = ActionRingEntry::new(RingAction::new(Action::Paste).expect("ring action"));
    for app in ["com.example.editor", "com.example.editorbeta"] {
        assert!(config.has_app_override("mouse", app), "{app}");
        assert_eq!(config.app_profile_for("mouse", app), Some("app:editor"));
        assert_eq!(
            config
                .effective_bindings("mouse", Some(app))
                .get(&ButtonId::Back),
            Some(&Binding::Single(Action::Undo)),
            "{app}"
        );
        assert_eq!(ring.profile_for(&config.apps, app), Some("app:editor"));
        assert_eq!(
            ring.effective_layout(&config.apps, Some(app))
                .slots
                .get(&ActionRingSlot::Top),
            Some(&paste),
            "{app}"
        );
    }
    let other = "com.example.other";
    assert!(!config.has_app_override("mouse", other));
    assert_eq!(config.app_profile_for("mouse", other), None);
    assert_eq!(ring.profile_for(&config.apps, other), None);
    assert_eq!(
        config
            .effective_bindings("mouse", Some(other))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::BrowserBack))
    );
}

#[test]
fn a_platform_profile_wins_over_an_alias_profile_for_the_same_app() {
    let config = load(&on_this_platform(
        r#"schema_version = 9
[devices.mouse.per_app_bindings."com.example.editor"]
Back = "Redo"
[devices.mouse.per_app_bindings."app:editor"]
Back = "Undo"
[apps.editor]
PLATFORM = "com.example.editor"
"#,
    ))
    .expect("both profiles must load");
    assert_eq!(
        config.app_profile_for("mouse", "com.example.editor"),
        Some("com.example.editor")
    );
    assert_eq!(
        config
            .effective_bindings("mouse", Some("com.example.editor"))
            .get(&ButtonId::Back),
        Some(&Binding::Single(Action::Redo))
    );
}

#[test]
fn a_profile_for_an_undefined_alias_fails_to_load() {
    for profile in [
        "[devices.mouse.per_app_bindings.\"app:editr\"]\nBack = \"Undo\"\n",
        "[devices.mouse.action_ring.per_app.\"app:editr\".slots]\nTop = { action = \"Copy\" }\n",
    ] {
        let source = format!("schema_version = 9\n{profile}[apps.editor]\nlinux = \"editor\"\n");
        assert_matches!(
            load(&source).expect_err("a misspelled alias must not load"),
            ConfigError::UndefinedAppAlias { device, key, .. }
                if device == "mouse" && key == "app:editr",
            "{profile}"
        );
    }
}

#[test]
fn an_invalid_alias_table_fails_to_parse() {
    assert_matches!(
        load("schema_version = 9\n[apps.chrome]\nwindows = \"exe:Chrome.exe\"\n")
            .expect_err("an upper-case Windows identifier must not load"),
        ConfigError::Parse { .. }
    );
}

#[test]
fn aliases_and_their_profiles_survive_a_save() {
    let config = load(&on_this_platform(
        r#"schema_version = 9
[devices.mouse.per_app_bindings."app:editor"]
Back = "Undo"
[apps.editor]
name = "Editor"
PLATFORM = ["com.example.editor", "com.example.editorbeta"]
"#,
    ))
    .expect("alias profile must load");
    let reloaded = write_and_read(&config);
    assert_eq!(reloaded.apps, config.apps);
    assert_eq!(
        reloaded.app_profile_for("mouse", "com.example.editorbeta"),
        Some("app:editor")
    );
    assert!(
        !toml::to_string(&Config::default())
            .expect("serialize")
            .contains("[apps"),
        "an empty alias table is not written"
    );
}

/// The desktop app never edits `[apps]`, so a save it makes for another
/// setting leaves the hand-written table, and its comments, in place.
#[test]
fn a_tracked_save_keeps_the_hand_written_alias_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let source = on_this_platform(
        r#"schema_version = 9
[devices.mouse.per_app_bindings."app:editor"]
Back = "Undo"
# Editor ships two bundles.
[apps.editor]
name = "Editor" # shown in the profile selector
PLATFORM = ["com.example.editor", "com.example.editorbeta"]
"#,
    );
    fs::write(&path, &source).expect("write config");
    let (mut config, mut file) = ConfigFile::load_from_path(&path).expect("load tracked");
    config.set_selected_device(Some("mouse".into()));
    file.save(&config).expect("save tracked");
    let saved = fs::read_to_string(&path).expect("read saved");
    for kept in [
        "# Editor ships two bundles.",
        "name = \"Editor\" # shown in the profile selector",
        "\"com.example.editorbeta\"",
        "[devices.mouse.per_app_bindings.\"app:editor\"]",
    ] {
        assert!(saved.contains(kept), "{kept:?} missing from:\n{saved}");
    }
    let reloaded = Config::load_from_path(&path).expect("saved config loads");
    assert_eq!(reloaded.apps, config.apps);
}
