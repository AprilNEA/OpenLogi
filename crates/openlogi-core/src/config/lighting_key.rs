//! Key names for per-key lighting colours.
//!
//! A name says where a key sits, not what its keycap prints: lighting zones
//! sit under switch positions, and a layout changes only the legends. Each
//! name stands for one USB HID keyboard usage. Keys that every layout prints
//! alike (letters aside) keep their usual name; the punctuation keys a layout
//! relabels are named after their US legend.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Every key name and its USB HID keyboard usage, in usage order.
const KEYS: &[(&str, u8)] = &[
    ("A", 0x04),
    ("B", 0x05),
    ("C", 0x06),
    ("D", 0x07),
    ("E", 0x08),
    ("F", 0x09),
    ("G", 0x0a),
    ("H", 0x0b),
    ("I", 0x0c),
    ("J", 0x0d),
    ("K", 0x0e),
    ("L", 0x0f),
    ("M", 0x10),
    ("N", 0x11),
    ("O", 0x12),
    ("P", 0x13),
    ("Q", 0x14),
    ("R", 0x15),
    ("S", 0x16),
    ("T", 0x17),
    ("U", 0x18),
    ("V", 0x19),
    ("W", 0x1a),
    ("X", 0x1b),
    ("Y", 0x1c),
    ("Z", 0x1d),
    ("1", 0x1e),
    ("2", 0x1f),
    ("3", 0x20),
    ("4", 0x21),
    ("5", 0x22),
    ("6", 0x23),
    ("7", 0x24),
    ("8", 0x25),
    ("9", 0x26),
    ("0", 0x27),
    ("Enter", 0x28),
    ("Esc", 0x29),
    ("Backspace", 0x2a),
    ("Tab", 0x2b),
    ("Space", 0x2c),
    ("Minus", 0x2d),
    ("Equal", 0x2e),
    ("LBracket", 0x2f),
    ("RBracket", 0x30),
    ("Backslash", 0x31),
    ("IsoHash", 0x32),
    ("Semicolon", 0x33),
    ("Quote", 0x34),
    ("Grave", 0x35),
    ("Comma", 0x36),
    ("Period", 0x37),
    ("Slash", 0x38),
    ("CapsLock", 0x39),
    ("F1", 0x3a),
    ("F2", 0x3b),
    ("F3", 0x3c),
    ("F4", 0x3d),
    ("F5", 0x3e),
    ("F6", 0x3f),
    ("F7", 0x40),
    ("F8", 0x41),
    ("F9", 0x42),
    ("F10", 0x43),
    ("F11", 0x44),
    ("F12", 0x45),
    ("PrintScreen", 0x46),
    ("ScrollLock", 0x47),
    ("Pause", 0x48),
    ("Insert", 0x49),
    ("Home", 0x4a),
    ("PageUp", 0x4b),
    ("Delete", 0x4c),
    ("End", 0x4d),
    ("PageDown", 0x4e),
    ("Right", 0x4f),
    ("Left", 0x50),
    ("Down", 0x51),
    ("Up", 0x52),
    ("NumLock", 0x53),
    ("KpSlash", 0x54),
    ("KpAsterisk", 0x55),
    ("KpMinus", 0x56),
    ("KpPlus", 0x57),
    ("KpEnter", 0x58),
    ("Kp1", 0x59),
    ("Kp2", 0x5a),
    ("Kp3", 0x5b),
    ("Kp4", 0x5c),
    ("Kp5", 0x5d),
    ("Kp6", 0x5e),
    ("Kp7", 0x5f),
    ("Kp8", 0x60),
    ("Kp9", 0x61),
    ("Kp0", 0x62),
    ("KpPeriod", 0x63),
    ("IsoBackslash", 0x64),
    ("Menu", 0x65),
    ("LCtrl", 0xe0),
    ("LShift", 0xe1),
    ("LAlt", 0xe2),
    ("LWin", 0xe3),
    ("RCtrl", 0xe4),
    ("RShift", 0xe5),
    ("RAlt", 0xe6),
    ("RWin", 0xe7),
];

/// A key position that can carry its own lighting colour, named in the
/// config as [`Self::name`] gives it.
///
/// Serializes as the name, so the config reads `keys = { Esc = "ff0000" }`.
/// Parsing ignores case. Ordered by HID usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LightingKey(u8);

impl LightingKey {
    /// The key whose USB HID keyboard usage is `usage`, if it has a name.
    #[must_use]
    pub fn from_usage(usage: u8) -> Option<Self> {
        KEYS.iter()
            .any(|&(_, known)| known == usage)
            .then_some(Self(usage))
    }

    /// The USB HID keyboard usage of this key.
    #[must_use]
    pub const fn usage(self) -> u8 {
        self.0
    }

    /// The name the config uses for this key.
    #[must_use]
    pub fn name(self) -> &'static str {
        KEYS.iter()
            .find(|&&(_, usage)| usage == self.0)
            .map_or("?", |&(name, _)| name)
    }

    /// Every named key, in usage order.
    pub fn all() -> impl Iterator<Item = Self> {
        KEYS.iter().map(|&(_, usage)| Self(usage))
    }
}

/// A key name the table does not list.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown key name {input:?} (expected a name such as A, 1, Esc, F1, LShift or Minus)")]
pub struct LightingKeyError {
    /// The rejected name.
    input: String,
}

impl FromStr for LightingKey {
    type Err = LightingKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        KEYS.iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(s))
            .map(|&(_, usage)| Self(usage))
            .ok_or_else(|| LightingKeyError { input: s.into() })
    }
}

impl TryFrom<String> for LightingKey {
    type Error = LightingKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<LightingKey> for String {
    fn from(key: LightingKey) -> Self {
        key.name().to_owned()
    }
}

impl fmt::Display for LightingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::{KEYS, LightingKey};

    #[test]
    fn names_parse_in_any_case_and_print_canonically() {
        for (input, usage, name) in [
            ("a", 0x04, "A"),
            ("ESC", 0x29, "Esc"),
            ("lshift", 0xe1, "LShift"),
            ("0", 0x27, "0"),
            ("isobackslash", 0x64, "IsoBackslash"),
        ] {
            let key: LightingKey = input.parse().expect("a known name");
            assert_eq!(key.usage(), usage);
            assert_eq!(key.to_string(), name);
        }
        assert!("Fn".parse::<LightingKey>().is_err());
        assert!("".parse::<LightingKey>().is_err());
    }

    #[test]
    fn the_table_has_one_name_per_usage_in_usage_order() {
        for pair in KEYS.windows(2) {
            assert!(pair[0].1 < pair[1].1, "{pair:?} out of order");
        }
        for (i, (name, _)) in KEYS.iter().enumerate() {
            assert!(
                KEYS[i + 1..]
                    .iter()
                    .all(|(other, _)| !other.eq_ignore_ascii_case(name)),
                "{name} listed twice"
            );
        }
        assert_eq!(LightingKey::all().count(), KEYS.len());
        assert_eq!(
            LightingKey::from_usage(0x04).map(LightingKey::name),
            Some("A")
        );
        assert_eq!(LightingKey::from_usage(0x01), None);
    }
}
