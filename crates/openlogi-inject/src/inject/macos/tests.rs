use core_graphics::event::CGEventFlags;
use openlogi_core::binding::Shortcut;

use super::{combo, held_key_event, hid_usage_to_macos};
use crate::inject::{HeldKey, HeldModifiers, KeyPhase};

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
