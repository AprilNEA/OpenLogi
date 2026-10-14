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

#[test]
fn app_direction_maps_survive_single_mode_and_reset_without_changing_global() {
    use crate::binding::GestureDirection;
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Up,
        Action::Copy,
    );
    let global = cfg.effective_bindings("2b042", None);
    cfg.set_per_app_gesture_direction(
        "2b042",
        "com.example.Editor",
        ButtonId::GestureButton,
        GestureDirection::Click,
        Action::Paste,
    );
    let custom = cfg.effective_bindings("2b042", Some("com.example.Editor"));
    let Binding::Gesture(map) = &custom[&ButtonId::GestureButton] else {
        panic!("app map missing")
    };
    assert_eq!(map[&GestureDirection::Up], Action::Copy);
    cfg.set_per_app_gesture_mode(
        "2b042",
        "com.example.Editor",
        ButtonId::GestureButton,
        false,
    );
    assert_eq!(
        cfg.effective_bindings("2b042", Some("com.example.Editor"))[&ButtonId::GestureButton],
        Binding::Single(Action::Paste)
    );
    assert!(
        cfg.per_app_gesture_overrides("2b042", "com.example.Editor")
            .is_some()
    );
    cfg.set_per_app_gesture_mode("2b042", "com.example.Editor", ButtonId::GestureButton, true);
    assert_eq!(
        cfg.effective_bindings("2b042", Some("com.example.Editor")),
        custom
    );
    assert_eq!(
        cfg.effective_bindings("2b042", Some("com.example.Other")),
        global
    );
    cfg.set_per_app_binding("2b042", "com.example.Editor", ButtonId::GestureButton, None);
    assert_eq!(
        cfg.effective_bindings("2b042", Some("com.example.Editor")),
        global
    );
    assert!(cfg.app_profiles("2b042").next().is_none());
}

#[test]
fn per_app_gesture_maps_roundtrip_without_changing_global_bindings() {
    let mut config = Config::default();
    config.set_binding(
        "unit:12345678",
        ButtonId::HapticPanel,
        Binding::Single(Action::ShowActionsRing),
    );
    config.set_per_app_gesture_mode(
        "unit:12345678",
        "com.example.App",
        ButtonId::HapticPanel,
        true,
    );
    config.set_per_app_gesture_direction(
        "unit:12345678",
        "com.example.App",
        ButtonId::HapticPanel,
        GestureDirection::Up,
        Action::Copy,
    );
    let reread = write_and_read(&config);
    assert_eq!(
        reread.devices["unit:12345678"].per_app_gesture_bindings,
        config.devices["unit:12345678"].per_app_gesture_bindings
    );
    assert_eq!(
        reread.devices["unit:12345678"].bindings,
        config.devices["unit:12345678"].bindings
    );
}
