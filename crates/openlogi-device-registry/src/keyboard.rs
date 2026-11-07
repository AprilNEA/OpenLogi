//! Known keyboards' per-key lighting zone maps.
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
}

const KEYBOARDS: &[Entry] = &[
    // G515 LS TKL: its Bluetooth, LIGHTSPEED and USB ids. Walked zone by zone
    // on a Spanish ISO unit; the Fn key sits in the right-GUI slot.
    Entry {
        model_ids: &[0xb389, 0x40b4, 0xc355],
        zones: PerKeyZones::UsageMinusThree,
    },
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

#[cfg(test)]
mod tests {
    use super::{PerKeyZones, per_key_zones};

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
