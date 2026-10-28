//! USB HID keyboard usages and the macOS virtual keycodes of the same keys.
//!
//! A virtual keycode names a physical key position, as a HID usage does, so
//! the two vocabularies are one table read in both directions: shortcut
//! synthesis turns a configured usage into the key a `CGEvent` presses, and
//! the virtual HID keyboard turns that key back into the usage its report
//! carries. The positions are US ANSI ones, the layout an unidentified
//! keyboard is translated with. The top row's keycodes stay with
//! [`FunctionKey`], which owns them for every process.

use openlogi_core::config::FunctionKey;

/// `Keyboard Escape`, the usage of [`FunctionKey::Esc`].
const ESCAPE: u8 = 0x29;

/// Every key outside the top row: `(HID usage, macOS virtual keycode)`.
///
/// Besides the keys a [`KeyboardUsage`](openlogi_core::binding::KeyboardUsage)
/// can name, it carries the rest of the keys a synthesized edge presses: the
/// ISO and JIS typing keys a keyboard layout can place a shortcut's character
/// on, and the left modifiers a held chord holds.
const KEYS: &[(u8, u16)] = &[
    (0x04, 0x00), // A
    (0x05, 0x0b), // B
    (0x06, 0x08), // C
    (0x07, 0x02), // D
    (0x08, 0x0e), // E
    (0x09, 0x03), // F
    (0x0a, 0x05), // G
    (0x0b, 0x04), // H
    (0x0c, 0x22), // I
    (0x0d, 0x26), // J
    (0x0e, 0x28), // K
    (0x0f, 0x25), // L
    (0x10, 0x2e), // M
    (0x11, 0x2d), // N
    (0x12, 0x1f), // O
    (0x13, 0x23), // P
    (0x14, 0x0c), // Q
    (0x15, 0x0f), // R
    (0x16, 0x01), // S
    (0x17, 0x11), // T
    (0x18, 0x20), // U
    (0x19, 0x09), // V
    (0x1a, 0x0d), // W
    (0x1b, 0x07), // X
    (0x1c, 0x10), // Y
    (0x1d, 0x06), // Z
    (0x1e, 0x12), // 1
    (0x1f, 0x13), // 2
    (0x20, 0x14), // 3
    (0x21, 0x15), // 4
    (0x22, 0x17), // 5
    (0x23, 0x16), // 6
    (0x24, 0x1a), // 7
    (0x25, 0x1c), // 8
    (0x26, 0x19), // 9
    (0x27, 0x1d), // 0
    (0x28, 0x24), // Return
    (0x2a, 0x33), // Backspace
    (0x2b, 0x30), // Tab
    (0x2c, 0x31), // Space
    (0x2d, 0x1b), // -
    (0x2e, 0x18), // =
    (0x2f, 0x21), // [
    (0x30, 0x1e), // ]
    (0x31, 0x2a), // \
    (0x33, 0x29), // ;
    (0x34, 0x27), // '
    (0x35, 0x32), // `
    (0x36, 0x2b), // ,
    (0x37, 0x2f), // .
    (0x38, 0x2c), // /
    (0x49, 0x72), // Insert, which macOS names Help (kVK_Help)
    (0x4a, 0x73), // Home
    (0x4b, 0x74), // Page Up
    (0x4c, 0x75), // Forward Delete
    (0x4d, 0x77), // End
    (0x4e, 0x79), // Page Down
    (0x4f, 0x7c), // Right Arrow
    (0x50, 0x7b), // Left Arrow
    (0x51, 0x7d), // Down Arrow
    (0x52, 0x7e), // Up Arrow
    (0x6f, 0x5a), // F20, one past the row `FunctionKey` covers
    (0x64, 0x0a), // ISO § (Non-US \ and |)
    (0x89, 0x5d), // JIS ¥ (International 3)
    (0x87, 0x5e), // JIS _ (International 1)
    (0xe0, 0x3b), // Left Control
    (0xe1, 0x38), // Left Shift
    (0xe2, 0x3a), // Left Option
    (0xe3, 0x37), // Left Command
];

fn keys() -> impl Iterator<Item = (u8, u16)> {
    KEYS.iter().copied()
}

/// The macOS virtual keycode of the key with HID usage `usage`.
pub(super) fn hid_usage_to_macos(usage: u8) -> Option<u16> {
    function_key(usage).map(FunctionKey::keycode).or_else(|| {
        keys()
            .find(|&(key_usage, _)| key_usage == usage)
            .map(|(_, vk)| vk)
    })
}

/// The HID usage of the key with macOS virtual keycode `vk`.
pub(super) fn macos_to_hid_usage(vk: u16) -> Option<u8> {
    FunctionKey::from_keycode(vk)
        .and_then(function_key_usage)
        .or_else(|| {
            keys()
                .find(|&(_, key_vk)| key_vk == vk)
                .map(|(usage, _)| usage)
        })
}

/// The top-row key with HID usage `usage`: Escape, then F1–F12 and F13–F19,
/// which the HID usage table numbers in two separate runs.
fn function_key(usage: u8) -> Option<FunctionKey> {
    match usage {
        ESCAPE => Some(FunctionKey::Esc),
        0x3a..=0x45 => FunctionKey::nth_f(u16::from(usage - 0x3a + 1)),
        0x68..=0x6e => FunctionKey::nth_f(u16::from(usage - 0x68 + 13)),
        _ => None,
    }
}

/// The HID usage of top-row key `key`, the inverse of [`function_key`].
fn function_key_usage(key: FunctionKey) -> Option<u8> {
    let n = FunctionKey::ALL.iter().position(|&each| each == key)?;
    let n = u8::try_from(n).ok()?;
    match n {
        0 => Some(ESCAPE),
        1..=12 => Some(0x3a + n - 1),
        13..=19 => Some(0x68 + n - 13),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use openlogi_core::binding::KeyboardUsage;
    use openlogi_core::config::FunctionKey;

    use super::{hid_usage_to_macos, keys, macos_to_hid_usage};
    use crate::inject::macos::keyboard_layout::typing_keys;

    #[test]
    fn every_shortcut_usage_round_trips() {
        for code in 0..=u8::MAX {
            if KeyboardUsage::try_new(code).is_err() {
                continue;
            }
            let vk = hid_usage_to_macos(code)
                .unwrap_or_else(|| panic!("usage {code:#04x} has no macOS keycode"));
            assert_eq!(macos_to_hid_usage(vk), Some(code), "usage {code:#04x}");
        }
    }

    /// A key a layout can put a shortcut's character on must have a usage,
    /// or the virtual keyboard could not press what the layout lookup chose.
    #[test]
    fn every_layout_typing_key_round_trips() {
        for vk in typing_keys() {
            let usage = macos_to_hid_usage(vk)
                .unwrap_or_else(|| panic!("keycode {vk:#04x} has no HID usage"));
            assert_eq!(hid_usage_to_macos(usage), Some(vk), "keycode {vk:#04x}");
        }
    }

    #[test]
    fn top_row_keys_use_the_function_key_keycodes() {
        for key in FunctionKey::ALL {
            let usage = macos_to_hid_usage(key.keycode())
                .unwrap_or_else(|| panic!("{key:?} has no HID usage"));
            assert_eq!(hid_usage_to_macos(usage), Some(key.keycode()), "{key:?}");
        }
        assert_eq!(macos_to_hid_usage(FunctionKey::Esc.keycode()), Some(0x29));
        assert_eq!(macos_to_hid_usage(FunctionKey::F12.keycode()), Some(0x45));
        assert_eq!(macos_to_hid_usage(FunctionKey::F13.keycode()), Some(0x68));
        assert_eq!(macos_to_hid_usage(FunctionKey::F19.keycode()), Some(0x6e));
    }

    /// A usage or keycode entered twice, or one shadowing a top-row keycode,
    /// would make one direction of the table disagree with the other.
    #[test]
    fn table_entries_are_unique_and_disjoint_from_the_top_row() {
        let mut usages = HashSet::new();
        let mut keycodes: HashSet<u16> = FunctionKey::ALL.iter().map(|k| k.keycode()).collect();
        for (usage, vk) in keys() {
            assert!(usages.insert(usage), "usage {usage:#04x} listed twice");
            assert!(keycodes.insert(vk), "keycode {vk:#04x} listed twice");
        }
    }

    #[test]
    fn modifier_keycodes_map_to_the_left_modifier_usages() {
        assert_eq!(macos_to_hid_usage(0x3b), Some(0xe0));
        assert_eq!(macos_to_hid_usage(0x38), Some(0xe1));
        assert_eq!(macos_to_hid_usage(0x3a), Some(0xe2));
        assert_eq!(macos_to_hid_usage(0x37), Some(0xe3));
    }
}
