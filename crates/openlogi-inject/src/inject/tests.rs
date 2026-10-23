//! Pure regression tests for input-output ownership and scroll quantization.

use openlogi_core::scroll::ScrollDelta;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use openlogi_core::binding::{HeldKeys, KeyCombo, MouseButton};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use super::{HeldKey, HeldOutput, HoldTransition, KeyPhase, OwnerTransition, SharedOutputOwners};
use super::{QuantizedScroll, ScrollQuantizer};

/// Synthetic high-resolution input: eight eighth-ticks must total exactly
/// one Windows/Linux wheel detent (120 raw units). This is deterministic
/// model data, not a hardware capture.
#[test]
fn fractional_frames_preserve_cumulative_wheel_distance() {
    let mut quantizer = ScrollQuantizer::default();
    let total = (0..8)
        .map(|_| {
            quantizer
                .quantize(ScrollDelta::wheel_ticks(0.0, 0.125), 120.0)
                .y
        })
        .sum::<i32>();
    assert_eq!(total, 120);
}

#[test]
fn opposing_fractional_input_cancels_without_rounding_drift() {
    let mut quantizer = ScrollQuantizer::default();
    let forward = quantizer.quantize(ScrollDelta::wheel_ticks(0.25, 0.0), 120.0);
    let backward = quantizer.quantize(ScrollDelta::wheel_ticks(-0.25, 0.0), 120.0);
    assert_eq!(forward, QuantizedScroll { x: 30, y: 0 });
    assert_eq!(backward, QuantizedScroll { x: -30, y: 0 });
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn combo(label: &str) -> KeyCombo {
    label.parse().expect("test shortcut must be valid")
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn shared_control_stays_down_until_its_last_chord_ends() {
    let control_a = combo("Ctrl+A");
    let control_b = combo("Ctrl+B");
    let mut output = HeldOutput::default();

    assert_eq!(
        output.transition(None, Some((&control_a, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Control, HeldKey::Key(control_a.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(None, Some((&control_b, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Key(control_b.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&control_a, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Key(control_a.key())],
            down: vec![],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&control_b, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Control, HeldKey::Key(control_b.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn shared_mouse_button_releases_after_its_final_owner() {
    let mut output = SharedOutputOwners::default();

    assert_eq!(
        output.transition(&[], &[MouseButton::Middle]),
        OwnerTransition {
            up: vec![],
            down: vec![MouseButton::Middle],
        }
    );
    assert_eq!(
        output.transition(&[], &[MouseButton::Middle]),
        OwnerTransition::default()
    );
    assert_eq!(
        output.transition(&[MouseButton::Middle], &[]),
        OwnerTransition::default()
    );
    assert_eq!(
        output.transition(&[MouseButton::Middle], &[]),
        OwnerTransition {
            up: vec![MouseButton::Middle],
            down: vec![],
        }
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn a_one_shot_mouse_click_cannot_release_an_active_drag() {
    let mut output = SharedOutputOwners::default();
    output.transition(&[], &[MouseButton::Middle]);
    let mut phases = Vec::new();
    output.tap_unowned(MouseButton::Middle, || {
        phases.extend([KeyPhase::Down, KeyPhase::Up]);
    });
    assert!(
        phases.is_empty(),
        "a tap must not lift the held middle button"
    );
    output.tap_unowned(MouseButton::Right, || {
        phases.extend([KeyPhase::Down, KeyPhase::Up]);
    });
    assert_eq!(phases, [KeyPhase::Down, KeyPhase::Up]);
    assert_eq!(
        output.transition(&[MouseButton::Middle], &[]),
        OwnerTransition {
            up: vec![MouseButton::Middle],
            down: vec![]
        }
    );
    phases.clear();
    output.tap_unowned(MouseButton::Middle, || {
        phases.extend([KeyPhase::Down, KeyPhase::Up]);
    });
    assert_eq!(phases, [KeyPhase::Down, KeyPhase::Up]);
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn replacing_a_mouse_button_with_itself_emits_no_edge() {
    let mut output = SharedOutputOwners::default();

    output.transition(&[], &[MouseButton::Left]);
    assert_eq!(
        output.transition(&[MouseButton::Left], &[MouseButton::Left]),
        OwnerTransition::default()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn different_mouse_buttons_have_independent_owners() {
    let mut output = SharedOutputOwners::default();

    output.transition(&[], &[MouseButton::Left]);
    assert_eq!(
        output.transition(&[], &[MouseButton::Right]),
        OwnerTransition {
            up: vec![],
            down: vec![MouseButton::Right],
        }
    );
    assert_eq!(
        output.transition(&[MouseButton::Left], &[]),
        OwnerTransition {
            up: vec![MouseButton::Left],
            down: vec![],
        }
    );
    assert_eq!(
        output.transition(&[MouseButton::Right], &[]),
        OwnerTransition {
            up: vec![MouseButton::Right],
            down: vec![],
        }
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn replacing_a_mouse_button_releases_the_old_edge_before_pressing_the_new_one() {
    let mut output = SharedOutputOwners::default();

    output.transition(&[], &[MouseButton::Left]);
    assert_eq!(
        output.transition(&[MouseButton::Left], &[MouseButton::Forward]),
        OwnerTransition {
            up: vec![MouseButton::Left],
            down: vec![MouseButton::Forward],
        }
    );
}

/// macOS presses F13 for Print Screen, so a Print Screen hold and an F13
/// hold own one key: the first release leaves F13 down for the other.
#[cfg(target_os = "macos")]
#[test]
fn print_screen_and_f13_holds_share_one_key() {
    let print_screen = combo("PrintScreen");
    let f13 = combo("F13");
    let mut output = HeldOutput::default();

    assert_eq!(
        output.transition(None, Some((&print_screen, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Key(f13.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(None, Some((&f13, HeldKeys::WholeChord))),
        HoldTransition::default()
    );
    assert_eq!(
        output.transition(Some((&print_screen, HeldKeys::WholeChord)), None),
        HoldTransition::default()
    );
    assert_eq!(
        output.transition(Some((&f13, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Key(f13.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn command_and_control_share_one_physical_output() {
    let command_a = combo("Cmd+A");
    let control_b = combo("Ctrl+B");
    let mut output = HeldOutput::default();

    output.transition(None, Some((&command_a, HeldKeys::WholeChord)));
    assert_eq!(
        output.transition(None, Some((&control_b, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Key(control_b.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&command_a, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Key(command_a.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn command_and_control_are_distinct_physical_outputs() {
    let command_a = combo("Cmd+A");
    let control_b = combo("Ctrl+B");
    let mut output = HeldOutput::default();

    output.transition(None, Some((&command_a, HeldKeys::WholeChord)));
    assert_eq!(
        output.transition(None, Some((&control_b, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Control, HeldKey::Key(control_b.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&command_a, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Command, HeldKey::Key(command_a.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn super_and_command_share_one_physical_output() {
    let super_a = combo("Super+A");
    let command_b = combo("Cmd+B");
    let mut output = HeldOutput::default();

    assert_eq!(
        output.transition(None, Some((&super_a, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Command, HeldKey::Key(super_a.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(None, Some((&command_b, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Key(command_b.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&super_a, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Key(super_a.key())],
            down: vec![],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&command_b, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Command, HeldKey::Key(command_b.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn super_plus_command_owns_the_physical_key_once() {
    let chord = combo("Cmd+Super+A");
    assert_eq!(
        super::held_keys(&chord, HeldKeys::WholeChord),
        vec![HeldKey::Command, HeldKey::Key(chord.key())]
    );
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn super_is_distinct_from_control() {
    let super_a = combo("Super+A");
    let command_b = combo("Cmd+B");
    let mut output = HeldOutput::default();

    assert_eq!(
        output.transition(None, Some((&super_a, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Super, HeldKey::Key(super_a.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(None, Some((&command_b, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Control, HeldKey::Key(command_b.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&super_a, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Super, HeldKey::Key(super_a.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(target_os = "macos")]
#[test]
fn shared_command_stays_down_until_its_last_chord_ends() {
    let command_a = combo("Cmd+A");
    let command_b = combo("Cmd+B");
    let mut output = HeldOutput::default();

    output.transition(None, Some((&command_a, HeldKeys::WholeChord)));
    assert_eq!(
        output.transition(None, Some((&command_b, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Key(command_b.key())],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&command_a, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Key(command_a.key())],
            down: vec![],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&command_b, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Command, HeldKey::Key(command_b.key())],
            down: vec![],
            tap: None,
        }
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn a_modifiers_only_hold_never_owns_its_ordinary_key() {
    let control_tab = combo("Ctrl+Tab");
    let mut output = HeldOutput::default();

    // The ordinary key is absent from both edges: it is tapped once as the
    // hold starts, not held, so it never enters the ownership map.
    assert_eq!(
        output.transition(None, Some((&control_tab, HeldKeys::ModifiersOnly))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Control],
            tap: Some(HeldKey::Key(control_tab.key())),
        }
    );
    assert_eq!(
        output.transition(Some((&control_tab, HeldKeys::ModifiersOnly)), None),
        HoldTransition {
            up: vec![HeldKey::Control],
            down: vec![],
            tap: None,
        }
    );
}

/// A whole-chord hold of the same chord must keep holding the ordinary
/// key: this is the difference the two actions exist to express.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn a_whole_chord_hold_of_the_same_chord_still_owns_its_ordinary_key() {
    let control_tab = combo("Ctrl+Tab");
    let mut output = HeldOutput::default();

    assert_eq!(
        output.transition(None, Some((&control_tab, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Control, HeldKey::Key(control_tab.key())],
            tap: None,
        }
    );
}

/// Two buttons, one ordinary key: a whole-chord hold keeps Tab down, so a
/// modifiers-only hold that starts meanwhile must not tap it. The tap's up
/// edge would release Tab under the first hold, which still owns it until
/// its own release.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn a_modifiers_only_hold_skips_its_tap_while_another_hold_owns_the_key() {
    let shift_tab = combo("Shift+Tab");
    let alt_tab = combo("Alt+Tab");
    let tab = HeldKey::Key(alt_tab.key());
    let mut output = HeldOutput::default();

    output.transition(None, Some((&shift_tab, HeldKeys::WholeChord)));
    assert_eq!(
        output.transition(None, Some((&alt_tab, HeldKeys::ModifiersOnly))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Alt],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&alt_tab, HeldKeys::ModifiersOnly)), None),
        HoldTransition {
            up: vec![HeldKey::Alt],
            down: vec![],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&shift_tab, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Shift, tab],
            down: vec![],
            tap: None,
        }
    );
    // With the owner gone, the next modifiers-only hold taps again.
    assert_eq!(
        output.transition(None, Some((&alt_tab, HeldKeys::ModifiersOnly))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Alt],
            tap: Some(tab),
        }
    );
}

/// The skip is about ownership, not about another hold merely existing:
/// a modifiers-only hold owns no ordinary key, so a second one still taps,
/// and a whole-chord hold that starts meanwhile still presses and owns Tab.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn a_modifiers_only_hold_never_blocks_another_hold_of_the_same_key() {
    let alt_tab = combo("Alt+Tab");
    let shift_tab = combo("Shift+Tab");
    let tab = HeldKey::Key(alt_tab.key());
    let mut output = HeldOutput::default();

    output.transition(None, Some((&alt_tab, HeldKeys::ModifiersOnly)));
    assert_eq!(
        output.transition(None, Some((&shift_tab, HeldKeys::ModifiersOnly))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Shift],
            tap: Some(tab),
        }
    );
    output.transition(Some((&shift_tab, HeldKeys::ModifiersOnly)), None);
    assert_eq!(
        output.transition(None, Some((&shift_tab, HeldKeys::WholeChord))),
        HoldTransition {
            up: vec![],
            down: vec![HeldKey::Shift, tab],
            tap: None,
        }
    );
    assert_eq!(
        output.transition(Some((&shift_tab, HeldKeys::WholeChord)), None),
        HoldTransition {
            up: vec![HeldKey::Shift, tab],
            down: vec![],
            tap: None,
        }
    );
}

/// A press whose whole-chord hold is replaced by a modifiers-only one of
/// the same key releases the key first, so the replacement owns no
/// conflict and taps as a fresh hold does.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn replacing_a_whole_chord_hold_frees_the_key_for_the_tap() {
    let shift_tab = combo("Shift+Tab");
    let alt_tab = combo("Alt+Tab");
    let tab = HeldKey::Key(alt_tab.key());
    let mut output = HeldOutput::default();

    output.transition(None, Some((&shift_tab, HeldKeys::WholeChord)));
    assert_eq!(
        output.transition(
            Some((&shift_tab, HeldKeys::WholeChord)),
            Some((&alt_tab, HeldKeys::ModifiersOnly))
        ),
        HoldTransition {
            up: vec![HeldKey::Shift, tab],
            down: vec![HeldKey::Alt],
            tap: Some(tab),
        }
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn replacement_preserves_shared_physical_outputs() {
    let old = combo("Ctrl+A");
    let new = combo("Ctrl+B");
    let mut output = HeldOutput::default();

    output.transition(None, Some((&old, HeldKeys::WholeChord)));
    assert_eq!(
        output.transition(
            Some((&old, HeldKeys::WholeChord)),
            Some((&new, HeldKeys::WholeChord))
        ),
        HoldTransition {
            up: vec![HeldKey::Key(old.key())],
            down: vec![HeldKey::Key(new.key())],
            tap: None,
        }
    );
}

#[test]
fn hid_usages_map_across_windows_key_categories() {
    use super::hid_usage_to_windows;

    assert_eq!(hid_usage_to_windows(0x04), Some(0x41)); // A
    assert_eq!(hid_usage_to_windows(0x1e), Some(0x31)); // 1
    assert_eq!(hid_usage_to_windows(0x3a), Some(0x70)); // F1
    assert_eq!(hid_usage_to_windows(0x6f), Some(0x83)); // F20
    assert_eq!(hid_usage_to_windows(0x50), Some(0x25)); // Left
    assert_eq!(hid_usage_to_windows(0x2c), Some(0x20)); // Space
    assert_eq!(hid_usage_to_windows(0x33), Some(0xba)); // Semicolon
    assert_eq!(hid_usage_to_windows(0x46), Some(0x2c)); // VK_SNAPSHOT
    assert_eq!(hid_usage_to_windows(0x49), Some(0x2d)); // Insert
    assert_eq!(hid_usage_to_windows(0xff), None);
}

/// Every key a custom shortcut can name has a Windows virtual key, or the
/// Windows backend drops the binding instead of pressing it.
#[test]
fn every_shortcut_usage_has_a_windows_virtual_key() {
    use openlogi_core::binding::KeyboardUsage;

    use super::hid_usage_to_windows;

    for code in 0..=u8::MAX {
        if KeyboardUsage::try_new(code).is_ok() {
            assert!(
                hid_usage_to_windows(code).is_some(),
                "usage {code:#04x} has no Windows virtual key"
            );
        }
    }
}
