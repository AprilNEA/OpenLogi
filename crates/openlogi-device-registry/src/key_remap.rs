//! Known keyboards' HID++ `0x1b05` key customisation sources.
//!
//! `0x1b05` names a remapped key by a source index into the keyboard's own
//! control list (`0x1b10`), whose order follows the key matrix rather than
//! any usage table. That is a per-product fact, recorded here from a key-by-
//! key walk: each index was remapped to a distinct letter and every key
//! pressed in turn.

/// A keyboard whose key sources are known, by the model ids `0x0003`
/// reports.
struct Entry {
    model_ids: &'static [u16],
    /// `(USB HID keyboard usage, source index)`, sorted by usage.
    sources: &'static [(u8, u8)],
}

/// G515 LS TKL (ISO). The Fn key, source `0x5a`, sends no usage and so has
/// no row; neither do the sources the ANSI and JIS layouts use.
const G515_SOURCES: &[(u8, u8)] = &[
    (0x04, 0x38), // A
    (0x05, 0x44), // B
    (0x06, 0x43), // C
    (0x07, 0x39), // D
    (0x08, 0x31), // E
    (0x09, 0x2a), // F
    (0x0a, 0x3a), // G
    (0x0b, 0x2b), // H
    (0x0c, 0x24), // I
    (0x0d, 0x3b), // J
    (0x0e, 0x2c), // K
    (0x0f, 0x3c), // L
    (0x10, 0x45), // M
    (0x11, 0x54), // N
    (0x12, 0x34), // O
    (0x13, 0x25), // P
    (0x14, 0x30), // Q
    (0x15, 0x22), // R
    (0x16, 0x29), // S
    (0x17, 0x32), // T
    (0x18, 0x33), // U
    (0x19, 0x53), // V
    (0x1a, 0x21), // W
    (0x1b, 0x52), // X
    (0x1c, 0x23), // Y
    (0x1d, 0x42), // Z
    (0x1e, 0x18), // 1
    (0x1f, 0x09), // 2
    (0x20, 0x19), // 3
    (0x21, 0x0a), // 4
    (0x22, 0x1a), // 5
    (0x23, 0x0b), // 6
    (0x24, 0x1b), // 7
    (0x25, 0x0c), // 8
    (0x26, 0x1c), // 9
    (0x27, 0x0d), // 0
    (0x28, 0x3e), // Enter
    (0x29, 0x00), // Esc
    (0x2a, 0x0f), // Backspace
    (0x2b, 0x20), // Tab
    (0x2c, 0x4a), // Space
    (0x2d, 0x1d), // Minus
    (0x2e, 0x0e), // Equal
    (0x2f, 0x35), // LBracket
    (0x30, 0x26), // RBracket
    (0x32, 0x2e), // IsoHash
    (0x33, 0x2d), // Semicolon
    (0x34, 0x3d), // Quote
    (0x35, 0x08), // Grave
    (0x36, 0x55), // Comma
    (0x37, 0x46), // Period
    (0x38, 0x56), // Slash
    (0x39, 0x28), // CapsLock
    (0x3a, 0x10), // F1
    (0x3b, 0x01), // F2
    (0x3c, 0x11), // F3
    (0x3d, 0x02), // F4
    (0x3e, 0x12), // F5
    (0x3f, 0x03), // F6
    (0x40, 0x13), // F7
    (0x41, 0x04), // F8
    (0x42, 0x14), // F9
    (0x43, 0x05), // F10
    (0x44, 0x15), // F11
    (0x45, 0x06), // F12
    (0x46, 0x16), // PrintScreen
    (0x47, 0x07), // ScrollLock
    (0x48, 0x17), // Pause
    (0x49, 0x1f), // Insert
    (0x4a, 0x27), // Home
    (0x4b, 0x37), // PageUp
    (0x4c, 0x2f), // Delete
    (0x4d, 0x3f), // End
    (0x4e, 0x4f), // PageDown
    (0x4f, 0x5e), // Right
    (0x50, 0x4d), // Left
    (0x51, 0x4e), // Down
    (0x52, 0x5d), // Up
    (0x64, 0x51), // IsoBackslash
    (0x65, 0x4c), // Menu
    (0xe0, 0x48), // LCtrl
    (0xe1, 0x41), // LShift
    (0xe2, 0x49), // LAlt
    (0xe3, 0x58), // LWin
    (0xe4, 0x5c), // RCtrl
    (0xe5, 0x57), // RShift
    (0xe6, 0x4b), // RAlt
];

const KEYBOARDS: &[Entry] = &[Entry {
    // G515 LS TKL: its Bluetooth, LIGHTSPEED and USB ids.
    model_ids: &[0xb389, 0x40b4, 0xc355],
    sources: G515_SOURCES,
}];

/// The key sources of the keyboard whose `0x0003` `getDeviceInfo` reports
/// `model_id`, if they are known. Any non-zero id the entry lists matches.
#[must_use]
pub fn key_sources(model_id: [u16; 3]) -> Option<KeySources> {
    KEYBOARDS
        .iter()
        .find(|entry| {
            model_id
                .iter()
                .any(|id| *id != 0 && entry.model_ids.contains(id))
        })
        .map(|entry| KeySources(entry.sources))
}

/// One keyboard's `0x1b05` source indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySources(&'static [(u8, u8)]);

impl KeySources {
    /// The source index of the key whose USB HID keyboard usage is `usage`,
    /// if the keyboard has that key.
    #[must_use]
    pub fn source(self, usage: u8) -> Option<u8> {
        self.0
            .binary_search_by_key(&usage, |&(known, _)| known)
            .ok()
            .map(|at| self.0[at].1)
    }

    /// Every `(usage, source)` pair, in usage order.
    #[must_use]
    pub fn all(self) -> &'static [(u8, u8)] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{G515_SOURCES, key_sources};

    #[test]
    fn the_g515_is_known_by_any_of_its_ids() {
        assert!(key_sources([0, 0x40b4, 0]).is_some());
        assert!(key_sources([0xb389, 0, 0]).is_some());
        assert!(key_sources([0, 0, 0]).is_none());
        assert!(key_sources([0xb38a, 0x40b5, 0xc356]).is_none());
    }

    #[test]
    fn the_g515_table_matches_its_walk() {
        let sources = key_sources([0, 0x40b4, 0]).expect("the G515");
        // Esc, F1, CapsLock, Menu, PrintScreen, Pause, PageDown.
        assert_eq!(sources.source(0x29), Some(0x00));
        assert_eq!(sources.source(0x3a), Some(0x10));
        assert_eq!(sources.source(0x39), Some(0x28));
        assert_eq!(sources.source(0x65), Some(0x4c));
        assert_eq!(sources.source(0x46), Some(0x16));
        assert_eq!(sources.source(0x48), Some(0x17));
        assert_eq!(sources.source(0x4e), Some(0x4f));
        // A usage the keyboard has no key for.
        assert_eq!(sources.source(0x53), None);
    }

    #[test]
    fn each_usage_and_source_appears_once_in_usage_order() {
        for pair in G515_SOURCES.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{pair:?} out of order");
        }
        for (i, (_, source)) in G515_SOURCES.iter().enumerate() {
            assert!(
                G515_SOURCES[i + 1..]
                    .iter()
                    .all(|(_, other)| other != source),
                "source {source:#04x} twice"
            );
        }
        assert_eq!(G515_SOURCES.len(), 87);
    }
}
