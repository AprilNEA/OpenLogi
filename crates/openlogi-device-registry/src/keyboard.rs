//! Known keyboards' per-key lighting zone maps and key positions.
//!
//! HID++ `0x8081` names its lighting zones by number and reports which
//! numbers are present, but nothing about which key each one lights. That is
//! a per-product fact, recorded here from `openlogi diag zones` walks.

/// How a keyboard's `0x8081` zone ids follow from USB HID keyboard usages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PerKeyZones {
    /// Usages `0x04..=0x65` (letters through the ISO `<>` key and Menu) light
    /// zone `usage - 3`; the eight modifiers `0xE0..=0xE7` light
    /// `0x68..=0x6F` in usage order.
    UsageMinusThree,
}

impl PerKeyZones {
    /// The zone that lights the key with HID keyboard `usage`, if the map
    /// has one.
    #[must_use]
    pub const fn zone(self, usage: u8) -> Option<u8> {
        match self {
            Self::UsageMinusThree => match usage {
                0x04..=0x65 => Some(usage - 3),
                0xe0..=0xe7 => Some(usage - 0xe0 + 0x68),
                _ => None,
            },
        }
    }
}

/// A keyboard whose zone map is known, by the model ids `0x0003` reports.
struct Entry {
    model_ids: &'static [u16],
    zones: PerKeyZones,
    /// Each key by HID usage: its horizontal centre, `0` at the left edge of
    /// the keys and `1000` at the right, and its row, `0` for the function
    /// keys, for effects drawn across them.
    keys: &'static [(u8, u16, u8)],
}

const KEYBOARDS: &[Entry] = &[
    // G515 LS TKL: its Bluetooth, LIGHTSPEED and USB ids. Walked zone by zone
    // on a Spanish ISO unit; the Fn key sits in the right-GUI slot.
    Entry {
        model_ids: &[0xb389, 0x40b4, 0xc355],
        zones: PerKeyZones::UsageMinusThree,
        keys: G515_KEYS,
    },
];

/// The G515 LS TKL's keys: horizontal centre and row, measured from the
/// `PERKEY_KEYBOARD` boxes of its G HUB depot image (the ISO Enter by its
/// outline, on the row of its top half).
const G515_KEYS: &[(u8, u16, u8)] = &[
    (0x04, 123, 3),
    (0x05, 371, 4),
    (0x06, 261, 4),
    (0x07, 234, 3),
    (0x08, 219, 2),
    (0x09, 289, 3),
    (0x0a, 343, 3),
    (0x0b, 399, 3),
    (0x0c, 494, 2),
    (0x0d, 454, 3),
    (0x0e, 509, 3),
    (0x0f, 563, 3),
    (0x10, 481, 4),
    (0x11, 425, 4),
    (0x12, 551, 2),
    (0x13, 605, 2),
    (0x14, 110, 2),
    (0x15, 275, 2),
    (0x16, 179, 3),
    (0x17, 330, 2),
    (0x18, 439, 2),
    (0x19, 316, 4),
    (0x1a, 165, 2),
    (0x1b, 206, 4),
    (0x1c, 385, 2),
    (0x1d, 152, 4),
    (0x1e, 82, 1),
    (0x1f, 137, 1),
    (0x20, 192, 1),
    (0x21, 246, 1),
    (0x22, 302, 1),
    (0x23, 357, 1),
    (0x24, 412, 1),
    (0x25, 467, 1),
    (0x26, 522, 1),
    (0x27, 578, 1),
    (0x28, 784, 2),
    (0x29, 27, 0),
    (0x2a, 770, 1),
    (0x2b, 42, 2),
    (0x2c, 378, 5),
    (0x2d, 633, 1),
    (0x2e, 687, 1),
    (0x2f, 660, 2),
    (0x30, 715, 2),
    (0x32, 728, 3),
    (0x33, 618, 3),
    (0x34, 673, 3),
    (0x35, 28, 1),
    (0x36, 536, 4),
    (0x37, 591, 4),
    (0x38, 646, 4),
    (0x39, 48, 3),
    (0x3a, 137, 0),
    (0x3b, 192, 0),
    (0x3c, 247, 0),
    (0x3d, 302, 0),
    (0x3e, 385, 0),
    (0x3f, 440, 0),
    (0x40, 495, 0),
    (0x41, 550, 0),
    (0x42, 632, 0),
    (0x43, 688, 0),
    (0x44, 744, 0),
    (0x45, 798, 0),
    (0x46, 862, 0),
    (0x47, 919, 0),
    (0x48, 974, 0),
    (0x49, 863, 1),
    (0x4a, 918, 1),
    (0x4b, 974, 1),
    (0x4c, 863, 2),
    (0x4d, 918, 2),
    (0x4e, 973, 2),
    (0x4f, 972, 5),
    (0x50, 863, 5),
    (0x51, 918, 5),
    (0x52, 919, 4),
    (0x64, 96, 4),
    (0x65, 722, 5),
    (0xe0, 33, 5),
    (0xe1, 34, 4),
    (0xe2, 171, 5),
    (0xe3, 103, 5),
    (0xe4, 790, 5),
    (0xe5, 749, 4),
    (0xe6, 584, 5),
    (0xe7, 653, 5),
];

/// The zone map of the keyboard whose `0x0003` `getDeviceInfo` reports
/// `model_id`. Any non-zero id the entry lists matches, since a keyboard
/// reports all of its transport ids whichever one it is connected over.
#[must_use]
pub fn per_key_zones(model_id: [u16; 3]) -> Option<PerKeyZones> {
    KEYBOARDS
        .iter()
        .find(|entry| {
            model_id
                .iter()
                .any(|id| *id != 0 && entry.model_ids.contains(id))
        })
        .map(|entry| entry.zones)
}

/// Where each key of the keyboard reporting `model_id` sits, as `(HID
/// usage, 0..=1000 from left to right, row from the top)`, for a known
/// keyboard.
#[must_use]
pub fn key_positions(model_id: [u16; 3]) -> Option<&'static [(u8, u16, u8)]> {
    KEYBOARDS
        .iter()
        .find(|entry| {
            model_id
                .iter()
                .any(|id| *id != 0 && entry.model_ids.contains(id))
        })
        .map(|entry| entry.keys)
}

#[cfg(test)]
mod tests {
    use super::{PerKeyZones, key_positions, per_key_zones};

    #[test]
    fn the_g515_places_every_mapped_key_once_across_the_board() {
        let keys = key_positions([0, 0x40b4, 0]).expect("the G515 is known");
        assert_eq!(keys.len(), 88);
        assert!(keys.iter().all(|&(_, x, row)| x <= 1000 && row <= 5));
        let esc = keys.iter().find(|(usage, ..)| *usage == 0x29);
        assert_eq!(
            esc.map(|&(_, x, row)| (x, row)),
            Some((27, 0)),
            "Esc, top left"
        );
        let space = keys.iter().find(|(usage, ..)| *usage == 0x2c);
        assert_eq!(space.map(|&(.., row)| row), Some(5), "Space, bottom row");
        assert!(
            keys.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "one entry per key, in usage order"
        );
        assert!(
            keys.iter()
                .all(|&(usage, ..)| PerKeyZones::UsageMinusThree.zone(usage).is_some())
        );
    }

    #[test]
    fn the_g515_is_known_by_any_of_its_ids() {
        let zones = Some(PerKeyZones::UsageMinusThree);
        assert_eq!(per_key_zones([0xb389, 0x40b4, 0xc355]), zones);
        assert_eq!(per_key_zones([0, 0x40b4, 0]), zones);
        assert_eq!(per_key_zones([0, 0, 0]), None);
        assert_eq!(per_key_zones([0xb38a, 0x40b5, 0xc356]), None);
    }

    #[test]
    fn the_g515_rule_matches_its_walk() {
        let zones = PerKeyZones::UsageMinusThree;
        // A, Esc, the ISO `<>` key, Menu.
        assert_eq!(zones.zone(0x04), Some(0x01));
        assert_eq!(zones.zone(0x29), Some(0x26));
        assert_eq!(zones.zone(0x64), Some(0x61));
        assert_eq!(zones.zone(0x65), Some(0x62));
        // Left Ctrl and the right-GUI slot, which is Fn on the G515.
        assert_eq!(zones.zone(0xe0), Some(0x68));
        assert_eq!(zones.zone(0xe7), Some(0x6f));
        // No usage maps to the reserved ids or past the modifiers.
        assert_eq!(zones.zone(0x00), None);
        assert_eq!(zones.zone(0x66), None);
        assert_eq!(zones.zone(0xe8), None);
    }
}
