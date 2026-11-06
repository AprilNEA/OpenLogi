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
    /// Each key's horizontal centre by HID usage, `0` at the left edge of
    /// the keys and `1000` at the right, for effects drawn across them.
    columns: &'static [(u8, u16)],
}

const KEYBOARDS: &[Entry] = &[
    // G515 LS TKL: its Bluetooth, LIGHTSPEED and USB ids. Walked zone by zone
    // on a Spanish ISO unit; the Fn key sits in the right-GUI slot.
    Entry {
        model_ids: &[0xb389, 0x40b4, 0xc355],
        zones: PerKeyZones::UsageMinusThree,
        columns: G515_COLUMNS,
    },
];

/// The G515 LS TKL's key centres, measured from the `PERKEY_KEYBOARD` boxes
/// of its G HUB depot image (the ISO Enter by its outline).
const G515_COLUMNS: &[(u8, u16)] = &[
    (0x04, 123),
    (0x05, 371),
    (0x06, 261),
    (0x07, 234),
    (0x08, 219),
    (0x09, 289),
    (0x0a, 343),
    (0x0b, 399),
    (0x0c, 494),
    (0x0d, 454),
    (0x0e, 509),
    (0x0f, 563),
    (0x10, 481),
    (0x11, 425),
    (0x12, 551),
    (0x13, 605),
    (0x14, 110),
    (0x15, 275),
    (0x16, 179),
    (0x17, 330),
    (0x18, 439),
    (0x19, 316),
    (0x1a, 165),
    (0x1b, 206),
    (0x1c, 385),
    (0x1d, 152),
    (0x1e, 82),
    (0x1f, 137),
    (0x20, 192),
    (0x21, 246),
    (0x22, 302),
    (0x23, 357),
    (0x24, 412),
    (0x25, 467),
    (0x26, 522),
    (0x27, 578),
    (0x28, 784),
    (0x29, 27),
    (0x2a, 770),
    (0x2b, 42),
    (0x2c, 378),
    (0x2d, 633),
    (0x2e, 687),
    (0x2f, 660),
    (0x30, 715),
    (0x32, 728),
    (0x33, 618),
    (0x34, 673),
    (0x35, 28),
    (0x36, 536),
    (0x37, 591),
    (0x38, 646),
    (0x39, 48),
    (0x3a, 137),
    (0x3b, 192),
    (0x3c, 247),
    (0x3d, 302),
    (0x3e, 385),
    (0x3f, 440),
    (0x40, 495),
    (0x41, 550),
    (0x42, 632),
    (0x43, 688),
    (0x44, 744),
    (0x45, 798),
    (0x46, 862),
    (0x47, 919),
    (0x48, 974),
    (0x49, 863),
    (0x4a, 918),
    (0x4b, 974),
    (0x4c, 863),
    (0x4d, 918),
    (0x4e, 973),
    (0x4f, 972),
    (0x50, 863),
    (0x51, 918),
    (0x52, 919),
    (0x64, 96),
    (0x65, 722),
    (0xe0, 33),
    (0xe1, 34),
    (0xe2, 171),
    (0xe3, 103),
    (0xe4, 790),
    (0xe5, 749),
    (0xe6, 584),
    (0xe7, 653),
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

/// Where each key of the keyboard reporting `model_id` sits across it, as
/// `(HID usage, 0..=1000 from left to right)`, for a known keyboard.
#[must_use]
pub fn key_columns(model_id: [u16; 3]) -> Option<&'static [(u8, u16)]> {
    KEYBOARDS
        .iter()
        .find(|entry| {
            model_id
                .iter()
                .any(|id| *id != 0 && entry.model_ids.contains(id))
        })
        .map(|entry| entry.columns)
}

#[cfg(test)]
mod tests {
    use super::{PerKeyZones, key_columns, per_key_zones};

    #[test]
    fn the_g515_places_every_mapped_key_once_across_the_board() {
        let columns = key_columns([0, 0x40b4, 0]).expect("the G515 is known");
        assert_eq!(columns.len(), 88);
        assert!(columns.iter().all(|&(_, x)| x <= 1000));
        let esc = columns.iter().find(|(usage, _)| *usage == 0x29);
        assert_eq!(esc.map(|&(_, x)| x), Some(27), "Esc sits at the left");
        assert!(
            columns.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "one entry per key, in usage order"
        );
        assert!(
            columns
                .iter()
                .all(|&(usage, _)| PerKeyZones::UsageMinusThree.zone(usage).is_some())
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
