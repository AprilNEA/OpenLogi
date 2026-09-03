//! Binding catalog / serde roundtrip tests.

use std::assert_matches;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::*;

// ── Roundtrip wrapper: defined here so it precedes any `let` statements ──

/// Minimal TOML-serializable wrapper used by `roundtrip`.
/// Defined at module scope to satisfy `clippy::items_after_statements`.
#[derive(Serialize, Deserialize)]
struct RoundtripWrapper {
    binding: BTreeMap<ButtonId, Action>,
}

// ── Catalog tests ─────────────────────────────────────────────────────────

#[test]
fn catalog_has_at_least_29_entries() {
    let catalog = Action::catalog();
    assert!(
        catalog.len() >= 29,
        "catalog has {} entries, need ≥ 29",
        catalog.len()
    );
}

#[test]
fn catalog_excludes_custom_shortcut() {
    let catalog = Action::catalog();
    for action in &catalog {
        assert!(
            !matches!(action, Action::CustomShortcut(_) | Action::HoldShortcut(_)),
            "catalog must not contain recorded shortcut actions"
        );
    }
}

#[test]
fn hold_shortcut_has_distinct_lifecycle_semantics() {
    let combo: KeyCombo = "Alt+Space".parse().expect("valid shortcut failed");
    let held = Action::HoldShortcut(combo.clone());

    assert_eq!(held.label(), "Hold Alt+Space");
    assert_eq!(held.category(), Category::Editing);
    assert_eq!(held.held_combo(), Some(&combo));
    assert_matches!(held.effect(), Effect::HeldKey(actual) if actual == &combo);
    assert_eq!(Action::CustomShortcut(combo).held_combo(), None);
}

#[test]
fn hold_shortcut_roundtrips_toml() {
    let action = Action::HoldShortcut("Alt+Space".parse().expect("valid shortcut failed"));
    assert_eq!(roundtrip(&action), action);
}

#[test]
fn power_user_action_labels_and_category() {
    assert_eq!(Action::TypeText("hi".into()).label(), "Type \"hi\"");
    assert_eq!(
        Action::RunAppleScript("osascript".into()).label(),
        "Run AppleScript"
    );
    assert_eq!(
        Action::RunShellCommand("echo hi".into()).label(),
        "Run Command"
    );
    // All three are power-user escape hatches: classed as Editing so a
    // hand-authored binding has a home group, but never in the default
    // catalog (asserted below).
    assert_eq!(Action::TypeText("x".into()).category(), Category::Editing);
    assert_eq!(
        Action::RunAppleScript("x".into()).category(),
        Category::Editing
    );
    assert_eq!(
        Action::RunShellCommand("x".into()).category(),
        Category::Editing
    );
}

#[test]
fn power_user_actions_excluded_from_catalog() {
    let cat = Action::catalog();
    assert!(cat.iter().all(|a| !matches!(
        a,
        Action::TypeText(_) | Action::RunAppleScript(_) | Action::RunShellCommand(_)
    )));
}

#[test]
fn power_user_actions_roundtrip_toml() {
    for action in [
        Action::TypeText("hello".into()),
        Action::RunAppleScript("beep".into()),
        Action::RunShellCommand("date".into()),
    ] {
        let toml = toml::to_string(&action).expect("serialize");
        let back: Action = toml::from_str(&toml).expect("deserialize");
        assert_eq!(action, back);
    }
}

#[test]
fn workflow_label_category_and_catalog_exclusion() {
    let wf = Action::Workflow(vec![
        WorkflowStep::TypeText("bite me".into()),
        WorkflowStep::Delay { millis: 5000 },
        WorkflowStep::PressKey("Enter".parse().expect("valid shortcut failed")),
    ]);
    assert_eq!(wf.label(), "Workflow (3 steps)");
    assert_eq!(wf.category(), Category::Editing);
    // Excluded from the default catalog like the other power-user actions.
    assert!(
        Action::catalog()
            .iter()
            .all(|a| !matches!(a, Action::Workflow(_)))
    );
}

#[test]
fn workflow_roundtrips_toml() {
    let wf = Action::Workflow(vec![
        WorkflowStep::TypeText("bite me".into()),
        WorkflowStep::Delay { millis: 5000 },
        WorkflowStep::PressKey("Shift+Enter".parse().expect("valid shortcut failed")),
        WorkflowStep::RunShellCommand("echo done".into()),
    ]);
    let toml = toml::to_string(&wf).expect("serialize");
    let back: Action = toml::from_str(&toml).expect("deserialize");
    assert_eq!(wf, back);
}

#[test]
fn actions_ring_is_available_to_normal_and_gesture_pickers() {
    assert_eq!(Action::ShowActionsRing.label(), "Actions Ring");
    assert_eq!(Action::ShowActionsRing.category(), Category::System);
    assert!(Action::catalog().contains(&Action::ShowActionsRing));
    assert_eq!(
        RingAction::new(Action::ShowActionsRing),
        Err(RingActionError::RecursiveTrigger)
    );
}

// ── Binding (merged model) serde routing ──────────────────────────────────

/// On-disk shape: a `ButtonId` → [`Binding`] map, as `DeviceConfig.bindings`
/// serializes it.
#[derive(Serialize, Deserialize)]
struct BindingWrapper {
    bindings: BTreeMap<ButtonId, Binding>,
}

fn binding_roundtrip(bindings: BTreeMap<ButtonId, Binding>) -> BTreeMap<ButtonId, Binding> {
    let toml = toml::to_string_pretty(&BindingWrapper { bindings }).expect("serialize");
    toml::from_str::<BindingWrapper>(&toml)
        .expect("deserialize")
        .bindings
}

#[test]
fn binding_single_roundtrips_including_payload_variants() {
    let mut bindings = BTreeMap::new();
    bindings.insert(ButtonId::Back, Binding::Single(Action::BrowserBack));
    bindings.insert(
        ButtonId::DpiToggle,
        Binding::Single(Action::SetDpiPreset(2)),
    );
    bindings.insert(
        ButtonId::Forward,
        Binding::Single(Action::CustomShortcut(
            "Cmd+P".parse().expect("valid shortcut failed"),
        )),
    );
    let back = binding_roundtrip(bindings);
    assert_eq!(back[&ButtonId::Back], Binding::Single(Action::BrowserBack));
    assert_eq!(
        back[&ButtonId::DpiToggle],
        Binding::Single(Action::SetDpiPreset(2))
    );
    assert_matches!(
        back[&ButtonId::Forward],
        Binding::Single(Action::CustomShortcut(_))
    );
}

#[test]
fn binding_gesture_roundtrips() {
    let mut map = BTreeMap::new();
    map.insert(GestureDirection::Up, Action::Copy);
    map.insert(GestureDirection::Click, Action::Paste);
    let mut bindings = BTreeMap::new();
    bindings.insert(ButtonId::GestureButton, Binding::Gesture(map.clone()));
    let back = binding_roundtrip(bindings);
    assert_eq!(back[&ButtonId::GestureButton], Binding::Gesture(map));
}

#[test]
fn binding_long_press_roundtrips_without_overlapping_other_table_shapes() {
    let binding = Binding::LongPress(LongPressBinding::new(Action::Copy, Action::MissionControl));
    let back = binding_roundtrip(BTreeMap::from([(ButtonId::Back, binding.clone())]));
    assert_eq!(back[&ButtonId::Back], binding);

    let toml = toml::to_string_pretty(&BindingWrapper { bindings: back }).expect("serialize");
    assert!(toml.contains("short = \"Copy\""));
    assert!(toml.contains("long = \"MissionControl\""));
    assert!(!toml.contains("LongPress"));
}

#[test]
fn binding_long_press_requires_exact_short_and_long_fields() {
    let missing_long = "[bindings.Back]\nshort = \"Copy\"";
    assert!(toml::from_str::<BindingWrapper>(missing_long).is_err());

    let unknown = "[bindings.Back]\nshort = \"Copy\"\nlong = \"Paste\"\nthreshold_ms = 900";
    assert!(toml::from_str::<BindingWrapper>(unknown).is_err());
}

/// The untagged-routing safety guard. A TOML table keyed by ANY
/// [`GestureDirection`] name must deserialize as [`Binding::Gesture`], never
/// [`Binding::Single`]. If a future [`Action`] payload variant is ever named
/// `Up`/`Down`/`Left`/`Right`/`Click`, the table would parse as `Single`
/// first and this test fails — catching the silent mis-route at CI time.
#[test]
fn binding_direction_keyed_table_routes_to_gesture() {
    for dir in GestureDirection::ALL {
        // `GestureDirection`'s serde key equals its `Display`/variant name.
        let toml = format!("bindings.GestureButton.{dir} = \"None\"");
        let parsed = toml::from_str::<BindingWrapper>(&toml).expect("deserialize");
        assert!(
            matches!(
                parsed.bindings[&ButtonId::GestureButton],
                Binding::Gesture(_)
            ),
            "a {dir}-keyed table must route to Gesture, not Single"
        );
    }
}

/// The collision case: a payload [`Action`] also serializes as a single-key
/// table, but untagged must keep it [`Binding::Single`] (it parses as a valid
/// externally-tagged `Action` before the `Gesture` arm is tried).
#[test]
fn binding_payload_action_stays_single() {
    let toml = "bindings.DpiToggle.SetDpiPreset = 2";
    let parsed = toml::from_str::<BindingWrapper>(toml).expect("deserialize");
    assert_eq!(
        parsed.bindings[&ButtonId::DpiToggle],
        Binding::Single(Action::SetDpiPreset(2))
    );
}

#[test]
fn binding_capture_region_roundtrips_as_single_string() {
    let toml = "bindings.Back = \"CaptureRegion\"";
    let parsed = toml::from_str::<BindingWrapper>(toml).expect("deserialize");
    assert_eq!(
        parsed.bindings[&ButtonId::Back],
        Binding::Single(Action::CaptureRegion)
    );

    let back = binding_roundtrip(parsed.bindings);
    assert_eq!(
        back[&ButtonId::Back],
        Binding::Single(Action::CaptureRegion)
    );
    assert_eq!(Action::CaptureRegion.label(), "Capture Region");
    assert_eq!(Action::CaptureRegion.category(), Category::System);
    assert!(Action::catalog().contains(&Action::CaptureRegion));
}

// ── TOML roundtrip ────────────────────────────────────────────────────────

/// Serialize then deserialize `action` through TOML, using a wrapper
/// struct because TOML requires a top-level table.
fn roundtrip(action: &Action) -> Action {
    let mut map: BTreeMap<ButtonId, Action> = BTreeMap::new();
    map.insert(ButtonId::Back, action.clone());
    let w = RoundtripWrapper { binding: map };
    let s = toml::to_string(&w).expect("serialize");
    let back: RoundtripWrapper = toml::from_str(&s).expect("deserialize");
    back.binding
        .into_values()
        .next()
        .expect("binding present after roundtrip")
}

#[test]
fn all_catalog_variants_roundtrip_toml() {
    for action in Action::catalog() {
        let back = roundtrip(&action);
        assert_eq!(action, back, "TOML roundtrip failed for {action:?}");
    }
}

#[test]
fn persisted_action_variant_names_are_stable() {
    let mut actions = Action::catalog();
    actions.extend([
        Action::SetDpiPreset(0),
        Action::CustomShortcut(
            "F1".parse()
                .unwrap_or_else(|error| panic!("valid shortcut failed: {error}")),
        ),
        Action::TypeText(String::new()),
        Action::RunAppleScript(String::new()),
        Action::RunShellCommand(String::new()),
        Action::Workflow(Vec::new()),
        Action::OpenApplication(
            ApplicationTarget::new("/Applications/OpenLogi.app", "OpenLogi")
                .unwrap_or_else(|error| panic!("valid target failed: {error}")),
        ),
        Action::HoldShortcut(
            "F2".parse()
                .unwrap_or_else(|error| panic!("valid shortcut failed: {error}")),
        ),
    ]);
    let mut actual: Vec<String> = actions
        .into_iter()
        .map(
            |action| match toml::Value::try_from(action).expect("serialize action") {
                toml::Value::String(name) => name,
                toml::Value::Table(table) if table.len() == 1 => table
                    .into_iter()
                    .next()
                    .map(|(key, _)| key)
                    .expect("one variant key"),
                value => panic!("unexpected action shape: {value:?}"),
            },
        )
        .collect();
    actual.sort();
    let mut expected = [
        "AppExpose",
        "BrowserBack",
        "BrowserForward",
        "CaptureRegion",
        "CloseTab",
        "Copy",
        "CustomShortcut",
        "Cut",
        "CycleDpiPresets",
        "Find",
        "HorizontalScrollLeft",
        "HorizontalScrollRight",
        "HoldShortcut",
        "LaunchpadShow",
        "LeftClick",
        "LockScreen",
        "MiddleClick",
        "MissionControl",
        "MouseBack",
        "MouseForward",
        "MuteVolume",
        "NewTab",
        "NextDesktop",
        "NextTab",
        "NextTrack",
        "None",
        "OpenApplication",
        "Paste",
        "PlayPause",
        "PrevTab",
        "PrevTrack",
        "PreviousDesktop",
        "Redo",
        "ReloadPage",
        "ReopenTab",
        "RightClick",
        "RunAppleScript",
        "RunShellCommand",
        "Save",
        "Screenshot",
        "ScrollDown",
        "ScrollUp",
        "SelectAll",
        "SetDpiPreset",
        "ShowActionsRing",
        "ShowDesktop",
        "Sleep",
        "ToggleSmartShift",
        "TypeText",
        "Undo",
        "VolumeDown",
        "VolumeUp",
        "Workflow",
    ];
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn custom_shortcut_roundtrips_toml() {
    let action = Action::CustomShortcut("Cmd+Shift+P".parse().expect("valid shortcut failed"));
    assert_eq!(roundtrip(&action), action);
}

#[test]
fn key_combo_rendered_label_is_canonical() {
    let combo: KeyCombo = "Cmd+Shift+P".parse().expect("valid shortcut failed");
    assert_eq!(combo.rendered_label(), "Cmd+Shift+P");
}

#[test]
fn key_combo_rendered_label_falls_back_to_modifiers_plus_key() {
    let combo: KeyCombo = "Cmd+Shift+P".parse().expect("valid shortcut failed");
    assert_eq!(combo.rendered_label(), "Cmd+Shift+P");
}

// ── Category tests ────────────────────────────────────────────────────────

#[test]
fn category_editing_variants() {
    assert_eq!(Action::Copy.category(), Category::Editing);
    assert_eq!(Action::Undo.category(), Category::Editing);
    assert_eq!(Action::SelectAll.category(), Category::Editing);
    assert_eq!(Action::Find.category(), Category::Editing);
    assert_eq!(Action::Save.category(), Category::Editing);
    assert_eq!(Action::Cut.category(), Category::Editing);
    assert_eq!(Action::Redo.category(), Category::Editing);
    assert_eq!(Action::Paste.category(), Category::Editing);
}

#[test]
fn category_browser_variants() {
    assert_eq!(Action::BrowserBack.category(), Category::Browser);
    assert_eq!(Action::BrowserForward.category(), Category::Browser);
    assert_eq!(Action::NewTab.category(), Category::Browser);
    assert_eq!(Action::CloseTab.category(), Category::Browser);
    assert_eq!(Action::ReopenTab.category(), Category::Browser);
    assert_eq!(Action::NextTab.category(), Category::Browser);
    assert_eq!(Action::PrevTab.category(), Category::Browser);
    assert_eq!(Action::ReloadPage.category(), Category::Browser);
}

#[test]
fn category_media_variants() {
    assert_eq!(Action::PlayPause.category(), Category::Media);
    assert_eq!(Action::NextTrack.category(), Category::Media);
    assert_eq!(Action::PrevTrack.category(), Category::Media);
    assert_eq!(Action::VolumeUp.category(), Category::Media);
    assert_eq!(Action::VolumeDown.category(), Category::Media);
    assert_eq!(Action::MuteVolume.category(), Category::Media);
}

#[test]
fn category_mouse_variants() {
    assert_eq!(Action::LeftClick.category(), Category::Mouse);
    assert_eq!(Action::RightClick.category(), Category::Mouse);
    assert_eq!(Action::MiddleClick.category(), Category::Mouse);
}

#[test]
fn category_dpi_variants() {
    assert_eq!(Action::CycleDpiPresets.category(), Category::Dpi);
    assert_eq!(Action::ToggleSmartShift.category(), Category::Dpi);
}

#[test]
fn category_scroll_variants() {
    assert_eq!(Action::ScrollUp.category(), Category::Scroll);
    assert_eq!(Action::ScrollDown.category(), Category::Scroll);
    assert_eq!(Action::HorizontalScrollLeft.category(), Category::Scroll);
    assert_eq!(Action::HorizontalScrollRight.category(), Category::Scroll);
}

#[test]
fn category_navigation_variants() {
    assert_eq!(Action::MissionControl.category(), Category::Navigation);
    assert_eq!(Action::AppExpose.category(), Category::Navigation);
    assert_eq!(Action::PreviousDesktop.category(), Category::Navigation);
    assert_eq!(Action::NextDesktop.category(), Category::Navigation);
    assert_eq!(Action::ShowDesktop.category(), Category::Navigation);
    assert_eq!(Action::LaunchpadShow.category(), Category::Navigation);
}

#[test]
fn category_system_variants() {
    assert_eq!(Action::LockScreen.category(), Category::System);
    assert_eq!(Action::Screenshot.category(), Category::System);
}

// ── Category label smoke test ─────────────────────────────────────────────

#[test]
fn category_labels_are_nonempty() {
    let categories = [
        Category::Editing,
        Category::Browser,
        Category::Media,
        Category::Mouse,
        Category::Dpi,
        Category::Scroll,
        Category::Navigation,
        Category::System,
    ];
    for cat in categories {
        assert!(!cat.label().is_empty(), "label empty for {cat:?}");
    }
}

// ── Default binding ───────────────────────────────────────────────────────

#[test]
fn dpi_toggle_default_is_cycle_dpi_presets() {
    assert_eq!(
        default_binding(ButtonId::DpiToggle),
        Action::CycleDpiPresets
    );
}

#[test]
fn haptic_panel_defaults_to_opening_the_actions_ring() {
    assert_eq!(
        default_binding(ButtonId::HapticPanel),
        Action::ShowActionsRing
    );
    assert!(ButtonId::ALL.contains(&ButtonId::HapticPanel));
}

#[test]
fn wheel_tilt_defaults_to_the_scroll_its_firmware_already_does() {
    // The seed has to match the native behavior on both sides: the capture
    // plan diverts a control only when its binding leaves the default, so any
    // other seed would divert an untouched tilt and kill horizontal scroll.
    assert_eq!(
        default_binding(ButtonId::WheelTiltLeft),
        Action::HorizontalScrollLeft
    );
    assert_eq!(
        default_binding(ButtonId::WheelTiltRight),
        Action::HorizontalScrollRight
    );
    for tilt in [ButtonId::WheelTiltLeft, ButtonId::WheelTiltRight] {
        assert!(ButtonId::ALL.contains(&tilt));
        // A tilt reaches the host over HID++ diversion only: the OS hook sees
        // a horizontal scroll, not a button, and it swipes nothing.
        assert!(!tilt.is_os_hook_button());
        assert!(!tilt.is_hidpp_gesture_source());
    }
}

#[test]
fn thumbwheel_defaults_match_normalised_native_direction() {
    // HID++ capture normalises the per-model firmware polarity to physical
    // forward/up. The defaults must then reproduce native horizontal scroll,
    // including when sensitivity alone causes the wheel to be diverted.
    assert_eq!(
        default_binding(ButtonId::ThumbwheelScrollUp),
        Action::HorizontalScrollLeft
    );
    assert_eq!(
        default_binding(ButtonId::ThumbwheelScrollDown),
        Action::HorizontalScrollRight
    );
}

// ── Effect classification ─────────────────────────────────────────────────
//
// `Action::effect()` is the platform-neutral IR `openlogi-inject`'s three
// backends dispatch on instead of matching `Action` directly. These tests
// don't re-derive the match (that would be tautological) — they assert the
// one property every caller actually depends on: every pickable action
// lowers to *some* real effect, and only `Action::None` is `Effect::None`.

#[test]
fn catalog_actions_lower_to_a_non_none_effect_except_none() {
    for action in Action::catalog() {
        let is_none_effect = matches!(action.effect(), Effect::None);
        assert_eq!(
            is_none_effect,
            action == Action::None,
            "{action:?} effect classification disagrees with being Action::None"
        );
    }
}

#[test]
fn power_user_and_device_side_actions_lower_to_the_expected_bucket() {
    let combo: KeyCombo = "Cmd+P"
        .parse()
        .unwrap_or_else(|error| panic!("valid shortcut failed: {error}"));
    let custom_shortcut = Action::CustomShortcut(combo);
    assert_matches!(custom_shortcut.effect(), Effect::Key(_));

    let hold_shortcut = Action::HoldShortcut(
        "Ctrl+Space"
            .parse()
            .unwrap_or_else(|error| panic!("valid shortcut failed: {error}")),
    );
    assert_matches!(hold_shortcut.effect(), Effect::HeldKey(_));

    let type_text = Action::TypeText("hi".into());
    assert_matches!(type_text.effect(), Effect::Text("hi"));

    let run_apple_script = Action::RunAppleScript("beep".into());
    assert_matches!(
        run_apple_script.effect(),
        Effect::Script(Script::AppleScript("beep"))
    );

    let run_shell_command = Action::RunShellCommand("date".into());
    assert_matches!(
        run_shell_command.effect(),
        Effect::Script(Script::ShellCommand("date"))
    );

    let workflow = Action::Workflow(vec![]);
    assert_matches!(workflow.effect(), Effect::Script(Script::Workflow(&[])));

    // DPI/SmartShift/the Actions Ring/OpenApplication are all handled above
    // or beside the injector, never inside a backend's own dispatch.
    for action in [
        Action::CycleDpiPresets,
        Action::SetDpiPreset(2),
        Action::ToggleSmartShift,
        Action::ShowActionsRing,
    ] {
        assert_matches!(action.effect(), Effect::AgentSide);
    }
    let target = ApplicationTarget::new("/Applications/Safari.app", "")
        .unwrap_or_else(|error| panic!("valid target failed: {error}"));
    assert_matches!(Action::OpenApplication(target).effect(), Effect::AgentSide);
}

#[test]
fn scroll_actions_lower_to_unit_direction() {
    assert_eq!(Action::ScrollUp.effect(), Effect::Scroll { dx: 0, dy: 1 });
    assert_eq!(
        Action::ScrollDown.effect(),
        Effect::Scroll { dx: 0, dy: -1 }
    );
    assert_eq!(
        Action::HorizontalScrollLeft.effect(),
        Effect::Scroll { dx: -1, dy: 0 }
    );
    assert_eq!(
        Action::HorizontalScrollRight.effect(),
        Effect::Scroll { dx: 1, dy: 0 }
    );
}
