//! Gesture mode as a per-button shape: seeding, demoting, and what survives a save.

use super::*;

#[test]
fn set_gesture_direction_upgrades_single_to_gesture() {
    let mut cfg = Config::default();
    // Start from a Single binding, then bind a swipe direction.
    cfg.set_binding(
        "2b042",
        ButtonId::Back,
        Binding::Single(Action::BrowserBack),
    );
    cfg.set_gesture_direction("2b042", ButtonId::Back, GestureDirection::Up, Action::Copy);

    match cfg.stored_bindings("2b042").get(&ButtonId::Back) {
        Some(Binding::Gesture(map)) => {
            // The prior single action is preserved as the Click entry.
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&Action::BrowserBack)
            );
            assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
        }
        other => panic!("expected Gesture after upgrade, got {other:?}"),
    }
}

#[test]
fn set_gesture_direction_on_fresh_gesture_button_seeds_click() {
    // Binding one direction on a never-configured gesture button must still
    // persist a `Click`, so the click projection is the canonical default
    // rather than `Action::None` (which reads as a no-op press).
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Up,
        Action::Copy,
    );

    match cfg.stored_bindings("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(map.get(&GestureDirection::Up), Some(&Action::Copy));
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&crate::binding::default_gesture_binding(
                    GestureDirection::Click
                )),
                "a fresh gesture button must seed a Click from its default"
            );
        }
        other => panic!("expected Gesture, got {other:?}"),
    }
}

#[test]
fn set_gesture_mode_seeds_a_fresh_button_with_full_directions() {
    let mut cfg = Config::default();
    // The dedicated HID++ gesture button gets the full default direction map.
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.stored_bindings("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            for dir in GestureDirection::ALL {
                assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
            }
        }
        other => panic!("expected full default gesture map, got {other:?}"),
    }

    // A fresh OS-hook button also gets all five directions, not just a Click:
    // its native action stays as Click, and the swipe arms are defaults — so
    // the GUI's shown defaults are exactly what the runtime dispatches.
    cfg.set_gesture_mode("2b042", ButtonId::Forward, true);
    match cfg.stored_bindings("2b042").get(&ButtonId::Forward) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&default_binding(ButtonId::Forward))
            );
            for dir in [
                GestureDirection::Up,
                GestureDirection::Down,
                GestureDirection::Left,
                GestureDirection::Right,
            ] {
                assert_eq!(map.get(&dir), Some(&default_gesture_binding(dir)));
            }
        }
        other => panic!("expected full gesture map for Forward, got {other:?}"),
    }
    // Both promotions coexist — no exclusivity.
    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert!(cfg.is_gesture_mode("2b042", ButtonId::Forward));
}

#[test]
fn gesture_state_roundtrips_through_shapes_without_the_owner_field() {
    // Since v4 the binding shape is the whole persisted truth: gesture
    // state survives a save/load cycle with no `gesture_owner` scalar in
    // the document.
    let mut cfg = Config::default();
    cfg.set_gesture_mode("2b042", ButtonId::Back, true);
    cfg.set_gesture_mode("4082d", ButtonId::GestureButton, false);

    let parsed = write_and_read(&cfg);
    assert!(parsed.is_gesture_mode("2b042", ButtonId::Back));
    assert!(
        parsed.is_gesture_mode("2b042", ButtonId::GestureButton),
        "the dedicated button's default gesture mode is untouched by Back's promotion"
    );
    assert!(parsed.gesture_mode_buttons("4082d").is_empty());

    let body = toml::to_string_pretty(&cfg).expect("serialize");
    assert!(!body.contains("gesture_owner"), "got: {body}");
}

#[test]
fn invalid_gesture_owner_string_is_tolerated_not_fatal() {
    // A hand-edit typo in gesture_owner must NOT fail the whole-document parse
    // (which would revert every device's settings to defaults). It degrades
    // to "infer" while the rest of the device config survives.
    let toml = "\
schema_version = 2

[devices.2b042]
gesture_owner = \"bogus\"

[devices.2b042.bindings]
Back = \"Copy\"
";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    fs::write(&path, toml).expect("write");

    let cfg =
        Config::load_from_path(&path).expect("an invalid gesture_owner must not fail the load");
    // The rest of the device config survived...
    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::Back),
        Some(&Binding::Single(Action::Copy))
    );
    // ...and the bad owner degraded to inference (HID++ button default here),
    // so the dedicated button keeps its default gesture mode.
    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
}

// ── Shape-driven gesture mode (the owner lock removed) ──

#[test]
fn gesture_mode_is_per_button_and_not_exclusive() {
    // The owner lock is gone: promoting a second button must not demote the
    // dedicated gesture button's default gesture mode.
    let mut cfg = Config::default();
    cfg.set_gesture_mode("2b042", ButtonId::MiddleClick, true);

    assert!(cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert!(cfg.is_gesture_mode("2b042", ButtonId::MiddleClick));
    let buttons = cfg.gesture_mode_buttons("2b042");
    assert!(
        buttons.contains(&ButtonId::GestureButton),
        "got: {buttons:?}"
    );
    assert!(buttons.contains(&ButtonId::MiddleClick), "got: {buttons:?}");
}

#[test]
fn set_gesture_mode_on_keeps_click_and_seeds_directions() {
    // Promoting a single-bound button keeps its action as the Click entry
    // and seeds every swipe arm, so the button exposes the full five-way set.
    let mut cfg = Config::default();
    cfg.set_binding("2b042", ButtonId::Back, Binding::Single(Action::Copy));
    cfg.set_gesture_mode("2b042", ButtonId::Back, true);

    let bindings = cfg.stored_bindings("2b042");
    let Some(Binding::Gesture(map)) = bindings.get(&ButtonId::Back) else {
        panic!(
            "expected a gesture binding, got {:?}",
            bindings.get(&ButtonId::Back)
        );
    };
    assert_eq!(map.get(&GestureDirection::Click), Some(&Action::Copy));
    for dir in [
        GestureDirection::Up,
        GestureDirection::Down,
        GestureDirection::Left,
        GestureDirection::Right,
    ] {
        assert_eq!(
            map.get(&dir),
            Some(&default_gesture_binding(dir)),
            "unseeded arm {dir:?}"
        );
    }
}

#[test]
fn set_gesture_mode_off_demotes_to_the_click_action() {
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Click,
        Action::Paste,
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);

    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));
    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::GestureButton),
        Some(&Binding::Single(Action::Paste))
    );
}

#[test]
fn set_gesture_mode_off_without_click_falls_back_to_the_default() {
    // A sparse hand-edited map with no Click must not demote to a dead
    // Single(None) — the button falls back to its canonical single default.
    let mut cfg = Config::default();
    let mut map = BTreeMap::new();
    map.insert(GestureDirection::Up, Action::Copy);
    cfg.set_binding("2b042", ButtonId::Back, Binding::Gesture(map));
    cfg.set_gesture_mode("2b042", ButtonId::Back, false);

    assert_eq!(
        cfg.stored_bindings("2b042").get(&ButtonId::Back),
        Some(&Binding::Single(Action::MouseBack))
    );
}

#[test]
fn off_then_on_restores_customized_swipe_arms() {
    // Turning gesture mode off must not destroy the user's four customized
    // swipe arms: re-enabling restores the map exactly as it was — the
    // guarantee the owner-lock model gave via dormant maps.
    let mut cfg = Config::default();
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Up,
        Action::Copy,
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);
    assert!(!cfg.is_gesture_mode("2b042", ButtonId::GestureButton));

    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.stored_bindings("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Up),
                Some(&Action::Copy),
                "the customized arm survives an off/on round trip"
            );
        }
        other => panic!("expected the restored gesture map, got {other:?}"),
    }
}

#[test]
fn re_promoting_a_genuine_single_keeps_it_as_click() {
    // A user's deliberate Single that happens to equal the button's
    // canonical default must not be mistaken for the pinned-off marker:
    // promoting keeps their action as the Click arm instead of resetting
    // the whole binding to the canonical default map.
    let mut cfg = Config::default();
    cfg.set_binding(
        "2b042",
        ButtonId::GestureButton,
        Binding::Single(default_binding(ButtonId::GestureButton)),
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match cfg.stored_bindings("2b042").get(&ButtonId::GestureButton) {
        Some(Binding::Gesture(map)) => {
            assert_eq!(
                map.get(&GestureDirection::Click),
                Some(&default_binding(ButtonId::GestureButton)),
                "the user's explicit action stays as Click"
            );
        }
        other => panic!("expected a gesture binding, got {other:?}"),
    }
}

#[test]
fn disabled_gesture_maps_survive_a_save_load_cycle() {
    let mut cfg = Config::default();
    cfg.set_gesture_direction(
        "2b042",
        ButtonId::GestureButton,
        GestureDirection::Down,
        Action::Paste,
    );
    cfg.set_gesture_mode("2b042", ButtonId::GestureButton, false);

    let mut restored = write_and_read(&cfg);
    restored.set_gesture_mode("2b042", ButtonId::GestureButton, true);
    match restored
        .stored_bindings("2b042")
        .get(&ButtonId::GestureButton)
    {
        Some(Binding::Gesture(map)) => {
            assert_eq!(map.get(&GestureDirection::Down), Some(&Action::Paste));
        }
        other => panic!("expected the persisted stash restored, got {other:?}"),
    }
}
