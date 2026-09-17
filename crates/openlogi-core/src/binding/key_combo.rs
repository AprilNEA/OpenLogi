//! Platform-neutral keyboard shortcut vocabulary and parser.

use std::str::FromStr;

use nutype::nutype;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

const MOD_COMMAND: u8 = 1 << 0;
const MOD_SHIFT: u8 = 1 << 1;
const MOD_CONTROL: u8 = 1 << 2;
const MOD_OPTION: u8 = 1 << 3;
const MOD_FN: u8 = 1 << 4;
const ALL_MODIFIERS: u8 = MOD_COMMAND | MOD_SHIFT | MOD_CONTROL | MOD_OPTION | MOD_FN;

/// USB HID keyboard usage supported by custom shortcuts.
///
/// Persisting a standard HID usage keeps the config independent of macOS
/// virtual keys, Linux evdev codes, and Windows virtual-key codes. Unknown
/// values are rejected during deserialization rather than silently ignored.
#[nutype(
    const_fn,
    validate(with = validate_keyboard_usage, error = KeyboardUsageError),
    derive(Clone, Copy, Debug, PartialEq, Eq, Hash, TryFrom, Into, Serialize, Deserialize),
)]
pub struct KeyboardUsage(u8);

impl KeyboardUsage {
    /// Raw USB HID usage ID for platform injection backends.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.into_inner()
    }

    /// Canonical name of this ordinary key for keyboard pickers.
    #[must_use]
    pub fn label(self) -> String {
        let code = self.into_inner();
        match code {
            0x04..=0x1d => char::from(b'A' + code - 0x04).to_string(),
            0x1e..=0x26 => char::from(b'1' + code - 0x1e).to_string(),
            0x27 => "0".to_string(),
            0x28 => "Enter".to_string(),
            0x29 => "Escape".to_string(),
            0x2a => "Backspace".to_string(),
            0x2b => "Tab".to_string(),
            0x2c => "Space".to_string(),
            0x2d => "-".to_string(),
            0x2e => "=".to_string(),
            0x2f => "[".to_string(),
            0x30 => "]".to_string(),
            0x31 => "\\".to_string(),
            0x33 => ";".to_string(),
            0x34 => "'".to_string(),
            0x35 => "`".to_string(),
            0x36 => ",".to_string(),
            0x37 => ".".to_string(),
            0x38 => "/".to_string(),
            0x3a..=0x45 => format!("F{}", code - 0x3a + 1),
            0x4a => "Home".to_string(),
            0x4b => "PageUp".to_string(),
            0x4c => "Delete".to_string(),
            0x4d => "End".to_string(),
            0x4e => "PageDown".to_string(),
            0x4f => "Right".to_string(),
            0x50 => "Left".to_string(),
            0x51 => "Down".to_string(),
            0x52 => "Up".to_string(),
            0x68..=0x6f => format!("F{}", code - 0x68 + 13),
            _ => format!("Usage 0x{code:02X}"),
        }
    }
}

/// Unsupported USB HID usage found in a shortcut payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("unsupported keyboard usage: {0:#04x}")]
pub struct KeyboardUsageError(pub u8);

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "nutype custom validators receive a reference to the wrapped value"
)]
const fn validate_keyboard_usage(value: &u8) -> Result<(), KeyboardUsageError> {
    if matches!(
        value,
        0x04..=0x31 | 0x33..=0x38 | 0x3a..=0x45 | 0x4a..=0x52 | 0x68..=0x6f
    ) {
        Ok(())
    } else {
        Err(KeyboardUsageError(*value))
    }
}

/// A platform-neutral keyboard chord.
///
/// Human-readable formats store the canonical text chord; binary IPC stores
/// validated modifier bits and a USB HID usage.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    modifiers: u8,
    key: Option<KeyboardUsage>,
}

#[derive(Serialize, Deserialize)]
struct KeyComboWire {
    modifiers: u8,
    // Zero encodes a modifier-only chord; ordinary HID usages keep their wire bytes.
    key: u8,
}

impl TryFrom<KeyComboWire> for KeyCombo {
    type Error = KeyComboParseError;

    fn try_from(value: KeyComboWire) -> Result<Self, Self::Error> {
        if value.modifiers & !ALL_MODIFIERS != 0 {
            return Err(KeyComboParseError::InvalidModifiers(value.modifiers));
        }
        let key = if value.key == 0 {
            if value.modifiers == 0 {
                return Err(KeyComboParseError::MissingKey);
            }
            None
        } else {
            Some(KeyboardUsage::try_from(value.key).map_err(KeyComboParseError::InvalidKey)?)
        };
        Ok(Self {
            modifiers: value.modifiers,
            key,
        })
    }
}

impl From<KeyCombo> for KeyComboWire {
    fn from(value: KeyCombo) -> Self {
        Self {
            modifiers: value.modifiers,
            key: value.key.map_or(0, KeyboardUsage::code),
        }
    }
}

impl Serialize for KeyCombo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.rendered_label())
        } else {
            KeyComboWire {
                modifiers: self.modifiers,
                key: self.key.map_or(0, KeyboardUsage::code),
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for KeyCombo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            String::deserialize(deserializer)?
                .parse()
                .map_err(de::Error::custom)
        } else {
            Self::try_from(KeyComboWire::deserialize(deserializer)?).map_err(de::Error::custom)
        }
    }
}

impl KeyCombo {
    /// The macOS Globe/Fn modifier, represented without a fake HID usage.
    pub const FN: Self = Self {
        modifiers: MOD_FN,
        key: None,
    };

    /// USB HID usage for the ordinary key, or `None` for a modifier-only chord.
    #[must_use]
    pub const fn key(&self) -> Option<KeyboardUsage> {
        self.key
    }

    /// Whether the chord includes Command/Meta (the cross-platform primary modifier).
    #[must_use]
    pub const fn has_command(&self) -> bool {
        self.modifiers & MOD_COMMAND != 0
    }

    /// Whether the chord includes Shift.
    #[must_use]
    pub const fn has_shift(&self) -> bool {
        self.modifiers & MOD_SHIFT != 0
    }

    /// Whether the chord includes Control.
    #[must_use]
    pub const fn has_control(&self) -> bool {
        self.modifiers & MOD_CONTROL != 0
    }

    /// Whether the chord includes Option/Alt.
    #[must_use]
    pub const fn has_option(&self) -> bool {
        self.modifiers & MOD_OPTION != 0
    }

    /// Whether the chord includes the macOS Globe/Fn modifier.
    #[must_use]
    pub const fn has_fn(&self) -> bool {
        self.modifiers & MOD_FN != 0
    }

    /// Canonical user-facing chord label.
    #[must_use]
    pub fn rendered_label(&self) -> String {
        let mut parts = Vec::new();
        if self.has_command() {
            parts.push("Cmd".to_string());
        }
        if self.has_control() {
            parts.push("Ctrl".to_string());
        }
        if self.has_option() {
            parts.push("Alt".to_string());
        }
        if self.has_shift() {
            parts.push("Shift".to_string());
        }
        if self.has_fn() {
            parts.push("Fn".to_string());
        }
        if let Some(key) = self.key {
            parts.push(key.label());
        }
        parts.join("+")
    }
}

/// Why a user-entered keyboard shortcut could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum KeyComboParseError {
    /// The shortcut field was blank.
    #[error("keyboard shortcut must not be empty")]
    Empty,
    /// The binary chord contains neither a modifier nor an ordinary key.
    #[error("keyboard shortcut must contain a key or modifier")]
    MissingKey,
    /// More than one non-modifier key was entered.
    #[error("keyboard shortcut must contain exactly one key")]
    MultipleKeys,
    /// A modifier or key name is not supported.
    #[error("unsupported shortcut token: {0}")]
    UnknownToken(String),
    /// Serialized modifier bits contain an unknown flag.
    #[error("unsupported shortcut modifier bits: {0:#04x}")]
    InvalidModifiers(u8),
    /// Serialized ordinary key is not a supported HID usage.
    #[error(transparent)]
    InvalidKey(KeyboardUsageError),
}

impl FromStr for KeyCombo {
    type Err = KeyComboParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(KeyComboParseError::Empty);
        }

        let symbolic_suffix = input
            .chars()
            .last()
            .is_some_and(|ch| matches!(ch, '⌘' | '⌃' | '⌥' | '⇧'));
        let input = input
            .replace('⌘', "Cmd+")
            .replace('⌃', "Ctrl+")
            .replace('⌥', "Alt+")
            .replace('⇧', "Shift+")
            .replace('🌐', "Fn");
        let input = if symbolic_suffix {
            input.trim_end_matches('+')
        } else {
            &input
        };
        let mut modifiers = 0;
        let mut key = None;
        for raw in input.split('+') {
            let token = raw.trim();
            if token.is_empty() {
                return Err(KeyComboParseError::UnknownToken(raw.to_string()));
            }
            if let Some(modifier) = parse_modifier(token) {
                modifiers |= modifier;
                continue;
            }
            if key.is_some() {
                return Err(KeyComboParseError::MultipleKeys);
            }
            key = Some(parse_key(token)?);
        }
        Ok(Self { modifiers, key })
    }
}

fn parse_modifier(token: &str) -> Option<u8> {
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "win" => Some(MOD_COMMAND),
        "shift" => Some(MOD_SHIFT),
        "ctrl" | "control" => Some(MOD_CONTROL),
        "alt" | "option" => Some(MOD_OPTION),
        "fn" | "globe" => Some(MOD_FN),
        _ => None,
    }
}

fn parse_key(token: &str) -> Result<KeyboardUsage, KeyComboParseError> {
    let lowercase = token.to_ascii_lowercase();
    let usage = if lowercase.len() == 1 {
        let character = lowercase.chars().next().unwrap_or_default();
        match character {
            'a'..='z' => 0x04 + u8::try_from(character as u32 - 'a' as u32).unwrap_or_default(),
            '1'..='9' => 0x1e + u8::try_from(character as u32 - '1' as u32).unwrap_or_default(),
            '0' => 0x27,
            '-' => 0x2d,
            '=' => 0x2e,
            '[' => 0x2f,
            ']' => 0x30,
            '\\' => 0x31,
            ';' => 0x33,
            '\'' => 0x34,
            '`' => 0x35,
            ',' => 0x36,
            '.' => 0x37,
            '/' => 0x38,
            _ => return Err(KeyComboParseError::UnknownToken(token.to_string())),
        }
    } else if let Some(number) = lowercase
        .strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
    {
        match number {
            1..=12 => 0x3a + number - 1,
            13..=20 => 0x68 + number - 13,
            _ => return Err(KeyComboParseError::UnknownToken(token.to_string())),
        }
    } else {
        match lowercase.as_str() {
            "enter" | "return" => 0x28,
            "escape" | "esc" => 0x29,
            "backspace" => 0x2a,
            "tab" => 0x2b,
            "space" => 0x2c,
            "home" => 0x4a,
            "pageup" | "page-up" => 0x4b,
            "delete" => 0x4c,
            "end" => 0x4d,
            "pagedown" | "page-down" => 0x4e,
            "right" => 0x4f,
            "left" => 0x50,
            "down" => 0x51,
            "up" => 0x52,
            _ => return Err(KeyComboParseError::UnknownToken(token.to_string())),
        }
    };
    KeyboardUsage::try_from(usage).map_err(|_| KeyComboParseError::UnknownToken(token.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_letters_and_navigation_keys() {
        let combo = "Cmd+Shift+P"
            .parse::<KeyCombo>()
            .expect("valid shortcut failed");
        assert!(combo.has_command());
        assert!(combo.has_shift());
        assert_eq!(combo.key().unwrap().code(), 0x13);
        assert_eq!(combo.rendered_label(), "Cmd+Shift+P");

        let combo = "Ctrl+Alt+Left"
            .parse::<KeyCombo>()
            .expect("valid shortcut failed");
        assert!(combo.has_control());
        assert!(combo.has_option());
        assert_eq!(combo.key().unwrap().code(), 0x50);
        assert_eq!(combo.rendered_label(), "Ctrl+Alt+Left");
    }

    #[test]
    fn a_uses_its_platform_neutral_hid_usage() {
        let combo = "Cmd+A".parse::<KeyCombo>().expect("valid shortcut failed");
        assert_eq!(combo.key().unwrap().code(), 0x04);
        assert_eq!(combo.rendered_label(), "Cmd+A");
    }

    #[test]
    fn rejects_missing_multiple_and_unknown_keys() {
        assert_eq!("Cmd+Shift".parse::<KeyCombo>().unwrap().key(), None);
        assert_eq!(
            "Cmd+P+K".parse::<KeyCombo>(),
            Err(KeyComboParseError::MultipleKeys)
        );
        assert!(matches!(
            "Cmd+Hyper".parse::<KeyCombo>(),
            Err(KeyComboParseError::UnknownToken(_))
        ));
    }

    #[test]
    fn rejects_unknown_serialized_usage_and_modifier_bits() {
        assert_eq!(
            KeyCombo::try_from(KeyComboWire {
                modifiers: 0,
                key: 255
            }),
            Err(KeyComboParseError::InvalidKey(KeyboardUsageError(255)))
        );
        assert_eq!(
            KeyCombo::try_from(KeyComboWire {
                modifiers: 0,
                key: 0
            }),
            Err(KeyComboParseError::MissingKey)
        );
        assert_eq!(
            KeyCombo::try_from(KeyComboWire {
                modifiers: 128,
                key: 0x04,
            }),
            Err(KeyComboParseError::InvalidModifiers(128))
        );
    }

    #[test]
    fn single_keys_modifier_only_chords_and_symbols_share_one_parser() {
        for (text, canonical, key, has_fn) in [
            ("T", "T", Some(0x17), false),
            ("Fn", "Fn", None, true),
            ("Globe", "Fn", None, true),
            ("🌐", "Fn", None, true),
            ("Ctrl", "Ctrl", None, false),
            ("Ctrl+Fn", "Ctrl+Fn", None, true),
            ("Fn+T", "Fn+T", Some(0x17), true),
            ("⌃⌥⇧T", "Ctrl+Alt+Shift+T", Some(0x17), false),
            ("⌃⌥⇧", "Ctrl+Alt+Shift", None, false),
        ] {
            let keys: KeyCombo = text.parse().unwrap();
            assert_eq!(keys.rendered_label(), canonical);
            assert_eq!(keys.key().map(KeyboardUsage::code), key);
            assert_eq!(keys.has_fn(), has_fn);
            assert_eq!(canonical.parse::<KeyCombo>().unwrap(), keys);
        }
        for invalid in ["", "+", "Ctrl+", "Ctrl++T", "T+U", "Hyper", "Fn+T+U"] {
            assert!(invalid.parse::<KeyCombo>().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn toml_uses_the_canonical_text_chord() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct Wrapper {
            shortcut: KeyCombo,
        }

        let combo = "Cmd+Shift+P"
            .parse::<KeyCombo>()
            .expect("valid shortcut failed");
        let wrapper = Wrapper { shortcut: combo };
        let encoded = toml::to_string(&wrapper).expect("shortcut serialization failed");
        assert_eq!(encoded, "shortcut = \"Cmd+Shift+P\"\n");
        assert_eq!(toml::from_str::<Wrapper>(&encoded), Ok(wrapper));
    }
}
