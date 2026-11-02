//! Key triggers and the global `[keyboard]` map.

use super::*;

#[test]
fn key_trigger_parses_bare_and_modified() {
    // Bare function key — F1 is macOS keycode 0x7A.
    let t: KeyTrigger = "f1".parse().expect("parse key trigger");
    assert_eq!(t.keycode, 0x7A);
    assert!(t.modifiers.is_empty());

    // Modifier-qualified, in any order, with aliases.
    let t: KeyTrigger = "shift+cmd+f5".parse().expect("parse key trigger");
    assert_eq!(t.keycode, 0x60); // F5
    assert!(t.modifiers.shift && t.modifiers.command);
    assert!(!t.modifiers.control && !t.modifiers.option);

    let t: KeyTrigger = "ctrl+alt+f2".parse().expect("parse key trigger");
    assert!(t.modifiers.control && t.modifiers.option);

    // Esc.
    assert_eq!(
        "esc"
            .parse::<KeyTrigger>()
            .expect("parse key trigger")
            .keycode,
        0x35
    );
}

#[test]
fn key_trigger_parses_and_displays_extended_function_keys() {
    let f13: KeyTrigger = "f13".parse().expect("parse key trigger");
    let f17: KeyTrigger = "command+f17".parse().expect("parse key trigger");
    let f19: KeyTrigger = "f19".parse().expect("parse key trigger");

    assert_eq!(f13.keycode, 0x69);
    assert_eq!(f17.keycode, 0x40);
    assert_eq!(f17.to_string(), "command+f17");
    assert_eq!(f19.keycode, 0x50);
    assert_eq!(f19.to_string(), "f19");
}

#[test]
fn key_trigger_rejects_unknown() {
    "f99"
        .parse::<KeyTrigger>()
        .expect_err("f99 is not a known key name");
    "shift+"
        .parse::<KeyTrigger>()
        .expect_err("a modifier with no key must be rejected");
    "".parse::<KeyTrigger>()
        .expect_err("an empty trigger must be rejected");
}

#[test]
fn keyboard_section_roundtrips_through_config() {
    let mut config = Config::default();
    config.keyboard.bindings.insert(
        "f1".parse().expect("parse key trigger"),
        Action::TypeText("hello".into()),
    );
    config.keyboard.bindings.insert(
        "shift+f2".parse().expect("parse key trigger"),
        Action::VolumeUp,
    );
    config.keyboard.bindings.insert(
        "f17".parse().expect("parse key trigger"),
        Action::MissionControl,
    );

    let roundtripped = write_and_read(&config);
    assert_eq!(roundtripped.keyboard.bindings.len(), 3);
    assert_eq!(
        roundtripped
            .keyboard
            .bindings
            .get(&"f1".parse::<KeyTrigger>().expect("parse key trigger")),
        Some(&Action::TypeText("hello".into()))
    );
    assert_eq!(
        roundtripped
            .keyboard
            .bindings
            .get(&"f17".parse::<KeyTrigger>().expect("parse key trigger")),
        Some(&Action::MissionControl)
    );
}

#[test]
fn set_keyboard_binding_inserts_and_clears() {
    let mut config = Config::default();
    let f1: KeyTrigger = "f1".parse().expect("parse key trigger");

    // Insert.
    config.set_keyboard_binding(f1.clone(), Some(Action::VolumeUp));
    assert_eq!(config.keyboard_bindings().get(&f1), Some(&Action::VolumeUp));
    assert_eq!(config.keyboard_bindings().len(), 1);

    // Overwrite.
    config.set_keyboard_binding(f1.clone(), Some(Action::MuteVolume));
    assert_eq!(
        config.keyboard_bindings().get(&f1),
        Some(&Action::MuteVolume)
    );
    assert_eq!(config.keyboard_bindings().len(), 1);

    // Clear via None.
    config.set_keyboard_binding(f1.clone(), None);
    assert!(config.keyboard_bindings().get(&f1).is_none());
    assert!(config.keyboard_bindings().is_empty());
}
