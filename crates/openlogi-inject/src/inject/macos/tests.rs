use std::sync::PoisonError;

use core_graphics::event::{CGEventFlags, CGEventType, EventField};
use openlogi_core::binding::{KeyboardUsage, Shortcut};

use super::{
    HELD_KEY_PRESSES, LOCK_SCREEN_CHORD, TYPED_TEXT_KEYS, combo, held_key_event,
    hid_usage_to_macos, key_event, text_events, typed_text_key,
};
use crate::inject::{HeldKey, HeldModifiers, KeyPhase, parse_shortcut};

#[test]
fn hid_usages_map_to_macos_virtual_keys() {
    assert_eq!(hid_usage_to_macos(0x04), Some(0x00));
    assert_eq!(hid_usage_to_macos(0x13), Some(0x23));
    assert_eq!(hid_usage_to_macos(0x50), Some(0x7b));
    assert_eq!(hid_usage_to_macos(0x3a), Some(0x7a));
    assert_eq!(hid_usage_to_macos(0x6f), Some(0x5a));
    assert_eq!(hid_usage_to_macos(0xff), None);
}

/// Pin a handful of representative `Shortcut -> KeyCombo` rows so an
/// edit to the table can't silently change what ⌘C sends. macOS and
/// Linux only overlap on the letter-key chords: both `BrowserBack` and
/// `Redo` differ across the three backends by design (see the module
/// doc on `combo`), so each backend pins its own rows independently.
#[test]
fn combo_table_pins_representative_shortcuts() {
    assert_eq!(combo(Shortcut::Copy).rendered_label(), "Cmd+C");
    assert_eq!(combo(Shortcut::Redo).rendered_label(), "Cmd+Shift+Z");
    assert_eq!(combo(Shortcut::BrowserBack).rendered_label(), "Cmd+[");
    assert_eq!(combo(Shortcut::NextTab).rendered_label(), "Ctrl+Tab");
    // hid_usage_to_macos must actually resolve every table entry, or a
    // `Shortcut` silently no-ops instead of pressing anything (see
    // `press_combo`'s warn-and-drop path). Iterates `Shortcut::ALL`
    // rather than a hand-copied list, so a newly added `Shortcut`
    // variant is checked here automatically instead of depending on
    // someone remembering to extend a second, independent list.
    for &shortcut in Shortcut::ALL {
        let key = combo(shortcut).key().code();
        assert!(
            hid_usage_to_macos(key).is_some(),
            "{shortcut:?} table entry has no macOS virtual-key mapping"
        );
    }
}

/// Lock Screen is the one chord outside the `combo` table, so the table loop
/// above never parses it. Pin it the same way: it must parse to the chord the
/// Apple menu shows, and its key must resolve to a macOS virtual key.
#[test]
fn lock_screen_chord_parses_and_resolves() {
    let chord = parse_shortcut(LOCK_SCREEN_CHORD);
    assert_eq!(chord.rendered_label(), "Cmd+Ctrl+Q");
    assert!(
        hid_usage_to_macos(chord.key().code()).is_some(),
        "the Lock Screen chord has no macOS virtual-key mapping"
    );
}

#[test]
fn held_edges_carry_the_aggregate_modifier_state() {
    let mut modifiers = HeldModifiers::default();
    let (_, flags) = held_key_event(HeldKey::Command, KeyPhase::Down, &mut modifiers)
        .expect("Command has a macOS virtual-key mapping");
    assert!(flags.contains(CGEventFlags::CGEventFlagCommand));

    let (_, flags) = held_key_event(HeldKey::Control, KeyPhase::Down, &mut modifiers)
        .expect("Control has a macOS virtual-key mapping");
    assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
    assert!(flags.contains(CGEventFlags::CGEventFlagControl));

    let key = combo(Shortcut::Copy).key();
    let (_, flags) = held_key_event(HeldKey::Key(key), KeyPhase::Up, &mut modifiers)
        .expect("Copy's key has a macOS virtual-key mapping");
    assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
    assert!(flags.contains(CGEventFlags::CGEventFlagControl));

    let (_, flags) = held_key_event(HeldKey::Command, KeyPhase::Up, &mut modifiers)
        .expect("Command has a macOS virtual-key mapping");
    assert!(!flags.contains(CGEventFlags::CGEventFlagCommand));
    assert!(flags.contains(CGEventFlags::CGEventFlagControl));
}

/// A key-down with no key-up leaves its key logically down, and an event
/// that inherits a held modifier types a shortcut instead of text.
#[test]
fn typed_text_releases_each_character_and_carries_no_modifier() {
    let events = text_events("aé", TYPED_TEXT_KEYS[0]).expect("keyboard events");
    let kinds: Vec<_> = events.iter().map(|event| event.get_type() as u32).collect();
    let (down, up) = (CGEventType::KeyDown as u32, CGEventType::KeyUp as u32);
    assert_eq!(kinds, [down, up, down, up]);
    let modifiers = CGEventFlags::CGEventFlagCommand
        | CGEventFlags::CGEventFlagControl
        | CGEventFlags::CGEventFlagShift
        | CGEventFlags::CGEventFlagAlternate;
    for event in &events {
        assert!(!event.get_flags().intersects(modifiers));
        assert_eq!(
            event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            crate::SYNTHETIC_EVENT_USER_DATA
        );
    }
}

/// A key-up on a key a held chord keeps down would end that hold early, so
/// typed text goes down on the next key no hold owns.
#[test]
fn typed_text_skips_a_key_a_held_chord_keeps_down() {
    let usage = KeyboardUsage::try_from(0x04).expect("0x04 is a valid keyboard usage");
    HELD_KEY_PRESSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(usage, TYPED_TEXT_KEYS[0]);
    let key = typed_text_key();
    HELD_KEY_PRESSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&usage);
    assert_eq!(key, Some(TYPED_TEXT_KEYS[1]));
}

/// The hook skips a key edge only by this tag, so every edge the backend
/// posts must carry it.
#[test]
fn posted_key_edges_carry_the_synthetic_tag() {
    for phase in [KeyPhase::Down, KeyPhase::Up] {
        let event =
            key_event(0x0c, CGEventFlags::CGEventFlagCommand, phase).expect("a keyboard event");
        assert_eq!(
            event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            crate::SYNTHETIC_EVENT_USER_DATA
        );
    }
}

#[test]
fn keypad_events_keep_their_identity_on_both_edges() {
    let modifiers = CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagShift;
    // Events.h's keypad keycodes, including Enter; F20 (0x5a) sits in the
    // middle of this range but is not a keypad key.
    for vk in [
        0x41, 0x43, 0x45, 0x4b, 0x4c, 0x4e, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
        0x5b, 0x5c,
    ] {
        for phase in [KeyPhase::Down, KeyPhase::Up] {
            let event = key_event(vk, modifiers, phase).expect("a keyboard event");
            assert_eq!(
                event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
                i64::from(vk)
            );
            assert_eq!(
                event.get_flags(),
                modifiers | CGEventFlags::CGEventFlagNumericPad,
                "keycode {vk:#04x}, {phase:?}"
            );
        }
    }
    // Main Return, digits, punctuation, and modifiers must not acquire Pad.
    for vk in [0x24, 0x12, 0x1d, 0x1b, 0x18, 0x2f, 0x2c, 0x38, 0x3b, 0x5a] {
        for phase in [KeyPhase::Down, KeyPhase::Up] {
            let event = key_event(vk, modifiers, phase).expect("a keyboard event");
            assert_eq!(event.get_flags(), modifiers, "keycode {vk:#04x}");
        }
    }
}
