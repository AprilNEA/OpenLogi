//! Platform-neutral keyboard shortcut vocabulary and parser.

use std::str::FromStr;

use nutype::nutype;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

const MOD_COMMAND: u8 = 1 << 0;
const MOD_SHIFT: u8 = 1 << 1;
const MOD_CONTROL: u8 = 1 << 2;
const MOD_OPTION: u8 = 1 << 3;
const ALL_MODIFIERS: u8 = MOD_COMMAND | MOD_SHIFT | MOD_CONTROL | MOD_OPTION;

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

    fn label(self) -> String {
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
            0x39 => "CapsLock".to_string(),
            0x3a..=0x45 => format!("F{}", code - 0x3a + 1),
            0x46 => "PrintScreen".to_string(),
            0x47 => "ScrollLock".to_string(),
            0x48 => "Pause".to_string(),
            0x49 => "Insert".to_string(),
            0x4a => "Home".to_string(),
            0x4b => "PageUp".to_string(),
            0x4c => "Delete".to_string(),
            0x4d => "End".to_string(),
            0x4e => "PageDown".to_string(),
            0x4f => "Right".to_string(),
            0x50 => "Left".to_string(),
            0x51 => "Down".to_string(),
            0x52 => "Up".to_string(),
            0x53 => "NumLock".to_string(),
            0x54 => "KpDivide".to_string(),
            0x55 => "KpMultiply".to_string(),
            0x56 => "KpMinus".to_string(),
            0x57 => "KpPlus".to_string(),
            0x58 => "KpEnter".to_string(),
            0x59..=0x61 => format!("Kp{}", code - 0x59 + 1),
            0x62 => "Kp0".to_string(),
            0x63 => "KpDecimal".to_string(),
            0x65 => "Menu".to_string(),
            0x67 => "KpEquals".to_string(),
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
        *value,
        0x04..=0x31 | 0x33..=0x63 | 0x65 | 0x67..=0x6f
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
    key: KeyboardUsage,
}

#[derive(Serialize, Deserialize)]
struct KeyComboWire {
    modifiers: u8,
    key: KeyboardUsage,
}

impl TryFrom<KeyComboWire> for KeyCombo {
    type Error = KeyComboParseError;

    fn try_from(value: KeyComboWire) -> Result<Self, Self::Error> {
        if value.modifiers & !ALL_MODIFIERS != 0 {
            return Err(KeyComboParseError::InvalidModifiers(value.modifiers));
        }
        Ok(Self {
            modifiers: value.modifiers,
            key: value.key,
        })
    }
}

impl From<KeyCombo> for KeyComboWire {
    fn from(value: KeyCombo) -> Self {
        Self {
            modifiers: value.modifiers,
            key: value.key,
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
                key: self.key,
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
    /// Build a chord from a HID keyboard report's modifier bitmap and usage ID.
    ///
    /// Left and right HID modifiers collapse into OpenLogi's four modifier
    /// bits. Returns `None` when `usage` is not a [`KeyboardUsage`] we persist.
    #[must_use]
    pub fn from_hid_report(modifiers: u8, usage: u8) -> Option<Self> {
        let key = KeyboardUsage::try_from(usage).ok()?;
        let mut bits = 0u8;
        if modifiers & (0x01 | 0x10) != 0 {
            bits |= MOD_CONTROL;
        }
        if modifiers & (0x02 | 0x20) != 0 {
            bits |= MOD_SHIFT;
        }
        if modifiers & (0x04 | 0x40) != 0 {
            bits |= MOD_OPTION;
        }
        if modifiers & (0x08 | 0x80) != 0 {
            bits |= MOD_COMMAND;
        }
        Some(Self {
            modifiers: bits,
            key,
        })
    }

    /// USB HID keyboard usage for the ordinary key.
    #[must_use]
    pub const fn key(&self) -> KeyboardUsage {
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

    /// Build a chord from a GUI key-capture event.
    ///
    /// `key` is the GPUI/xkb key name (`home`, `insert`, `subtract`, `6`),
    /// not the character the layout would type. AZERTY number-row punctuation
    /// (`-` on the 6 key) is mapped back to the physical HID usage so a G502
    /// binding injects KEY_6 rather than KEY_MINUS.
    #[must_use]
    pub fn from_captured(stroke: CapturedKeystroke<'_>) -> Option<Self> {
        if stroke.key.is_empty() || is_modifier_key_name(stroke.key) {
            return None;
        }
        let key = parse_key(remap_captured_key(stroke.key, stroke.layout_name)).ok()?;
        Some(Self {
            modifiers: stroke.modifiers,
            key,
        })
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
        parts.push(self.key.label());
        parts.join("+")
    }
}

/// One key-down from the settings app's shortcut recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedKeystroke<'a> {
    modifiers: u8,
    key: &'a str,
    layout_name: &'a str,
}

impl<'a> CapturedKeystroke<'a> {
    /// GPUI `Keystroke.key` plus the host layout name (`French`, `English (US)`, …).
    #[must_use]
    pub const fn new(key: &'a str, layout_name: &'a str) -> Self {
        Self {
            modifiers: 0,
            key,
            layout_name,
        }
    }

    /// Command/Meta/Super is down.
    #[must_use]
    pub const fn command(mut self, down: bool) -> Self {
        if down {
            self.modifiers |= MOD_COMMAND;
        }
        self
    }

    /// Shift is down.
    #[must_use]
    pub const fn shift(mut self, down: bool) -> Self {
        if down {
            self.modifiers |= MOD_SHIFT;
        }
        self
    }

    /// Control is down.
    #[must_use]
    pub const fn control(mut self, down: bool) -> Self {
        if down {
            self.modifiers |= MOD_CONTROL;
        }
        self
    }

    /// Option/Alt is down.
    #[must_use]
    pub const fn option(mut self, down: bool) -> Self {
        if down {
            self.modifiers |= MOD_OPTION;
        }
        self
    }
}

/// Why a user-entered keyboard shortcut could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum KeyComboParseError {
    /// The shortcut field was blank.
    #[error("keyboard shortcut must not be empty")]
    Empty,
    /// The shortcut contains modifiers but no ordinary key.
    #[error("keyboard shortcut must contain a key")]
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
}

impl FromStr for KeyCombo {
    type Err = KeyComboParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(KeyComboParseError::Empty);
        }

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
        let Some(key) = key else {
            return Err(KeyComboParseError::MissingKey);
        };
        Ok(Self { modifiers, key })
    }
}

fn parse_modifier(token: &str) -> Option<u8> {
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "win" => Some(MOD_COMMAND),
        "shift" => Some(MOD_SHIFT),
        "ctrl" | "control" => Some(MOD_CONTROL),
        "alt" | "option" => Some(MOD_OPTION),
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
    } else if let Some(number) = lowercase
        .strip_prefix("kp")
        .and_then(|number| number.parse::<u8>().ok())
    {
        match number {
            0 => 0x62,
            1..=9 => 0x59 + number - 1,
            _ => return Err(KeyComboParseError::UnknownToken(token.to_string())),
        }
    } else {
        match lowercase.as_str() {
            "enter" | "return" => 0x28,
            "escape" | "esc" => 0x29,
            "backspace" => 0x2a,
            "tab" => 0x2b,
            "space" => 0x2c,
            "capslock" | "caps" => 0x39,
            "printscreen" | "print-screen" => 0x46,
            "scrolllock" | "scroll-lock" => 0x47,
            "pause" => 0x48,
            "insert" | "ins" => 0x49,
            "home" => 0x4a,
            "pageup" | "page-up" => 0x4b,
            "delete" => 0x4c,
            "end" => 0x4d,
            "pagedown" | "page-down" => 0x4e,
            "right" => 0x4f,
            "left" => 0x50,
            "down" => 0x51,
            "up" => 0x52,
            "numlock" | "num-lock" => 0x53,
            "minus" => 0x2d,
            "kpdivide" | "kp-divide" | "kp_divide" | "divide" => 0x54,
            "kpmultiply" | "kp-multiply" | "kp_multiply" | "multiply" => 0x55,
            "kpminus" | "kp-minus" | "kp_minus" | "subtract" | "kp-subtract" | "kp_subtract" => {
                0x56
            }
            "kpplus" | "kp-plus" | "kp_plus" | "add" | "kp-add" | "kp_add" => 0x57,
            "kpenter" | "kp-enter" | "kp_enter" | "numpadenter" => 0x58,
            "kpdecimal" | "kp-decimal" | "kp_decimal" | "decimal" | "kp-separator"
            | "kp_separator" => 0x63,
            "menu" => 0x65,
            "kpequals" | "kp-equals" => 0x67,
            _ => return Err(KeyComboParseError::UnknownToken(token.to_string())),
        }
    };
    KeyboardUsage::try_from(usage).map_err(|_| KeyComboParseError::UnknownToken(token.to_string()))
}

fn is_modifier_key_name(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "control"
            | "ctrl"
            | "shift"
            | "alt"
            | "option"
            | "cmd"
            | "command"
            | "meta"
            | "win"
            | "super"
            | "fn"
            | "function"
    )
}

/// GPUI Linux reports the xkb keysym, so AZERTY's 6 key arrives as `-`.
/// Non-ASCII unshifted glyphs (`é`) arrive as xkb names (`eacute`).
fn remap_captured_key<'a>(key: &'a str, layout_name: &str) -> &'a str {
    if is_french_france_layout(layout_name) {
        return remap_french_number_row(key);
    }
    if is_belgian_layout(layout_name) {
        return remap_belgian_number_row(key);
    }
    key
}

fn remap_french_number_row(key: &str) -> &str {
    match key {
        "&" => "1",
        "é" | "eacute" => "2",
        "\"" => "3",
        "'" => "4",
        "(" => "5",
        "-" => "6",
        "è" | "egrave" => "7",
        "_" => "8",
        "ç" | "ccedilla" => "9",
        "à" | "agrave" => "0",
        ")" => "-",
        "²" | "twosuperior" => "`",
        _ => key,
    }
}

fn remap_belgian_number_row(key: &str) -> &str {
    match key {
        "&" => "1",
        "é" | "eacute" => "2",
        "\"" => "3",
        "'" => "4",
        "(" => "5",
        "§" | "section" => "6",
        "è" | "egrave" => "7",
        "!" => "8",
        "ç" | "ccedilla" => "9",
        "à" | "agrave" => "0",
        _ => key,
    }
}

fn is_french_france_layout(name: &str) -> bool {
    if is_belgian_layout(name) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("switzerland") || lower.contains("canada") {
        return false;
    }
    lower.contains("azerty")
        || lower == "fr"
        || lower == "french"
        || lower.starts_with("french ")
        || lower.starts_with("français")
}

fn is_belgian_layout(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("belgian") || lower.contains("belge") || lower == "be"
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
        assert_eq!(combo.key().code(), 0x13);
        assert_eq!(combo.rendered_label(), "Cmd+Shift+P");

        let combo = "Ctrl+Alt+Left"
            .parse::<KeyCombo>()
            .expect("valid shortcut failed");
        assert!(combo.has_control());
        assert!(combo.has_option());
        assert_eq!(combo.key().code(), 0x50);
        assert_eq!(combo.rendered_label(), "Ctrl+Alt+Left");
    }

    #[test]
    fn a_uses_its_platform_neutral_hid_usage() {
        let combo = "Cmd+A".parse::<KeyCombo>().expect("valid shortcut failed");
        assert_eq!(combo.key().code(), 0x04);
        assert_eq!(combo.rendered_label(), "Cmd+A");
    }

    #[test]
    fn hid_report_collapses_left_and_right_modifiers() {
        let combo = KeyCombo::from_hid_report(0x01 | 0x20 | 0x80, 0x04)
            .expect("0x04 is a valid keyboard usage");
        assert!(combo.has_control());
        assert!(combo.has_shift());
        assert!(combo.has_command());
        assert!(!combo.has_option());
        assert_eq!(combo.key().code(), 0x04);
        assert!(KeyCombo::from_hid_report(0, 0xff).is_none());
        let plus = KeyCombo::from_hid_report(0, 0x57).expect("keypad plus is a valid usage");
        assert_eq!(plus.rendered_label(), "KpPlus");
        assert_eq!("KpPlus".parse::<KeyCombo>(), Ok(plus));
    }

    fn captured(key: &str, layout: &str) -> KeyCombo {
        KeyCombo::from_captured(CapturedKeystroke::new(key, layout))
            .unwrap_or_else(|| panic!("captured {key:?} on {layout:?}"))
    }

    #[test]
    fn captured_named_keys_and_keypad_operators() {
        assert_eq!(captured("home", "English (US)").key().code(), 0x4a);
        assert_eq!(captured("insert", "English (US)").key().code(), 0x49);
        assert_eq!(captured("minus", "English (US)").key().code(), 0x2d);
        assert_eq!(captured("subtract", "English (US)").key().code(), 0x56);
        assert_eq!(captured("kp_subtract", "English (US)").key().code(), 0x56);
        assert_eq!(captured("add", "English (US)").key().code(), 0x57);
        assert_eq!(captured("6", "English (US)").key().code(), 0x23);
        assert_eq!(captured("kp6", "English (US)").key().code(), 0x5e);
        assert_eq!(captured("kp0", "English (US)").key().code(), 0x62);
        assert_eq!(captured("-", "English (US)").key().code(), 0x2d);
        assert_eq!(
            KeyCombo::from_captured(CapturedKeystroke::new("home", "English (US)").control(true),)
                .expect("Ctrl+Home")
                .rendered_label(),
            "Ctrl+Home"
        );
    }

    #[test]
    fn azerty_number_row_punctuation_maps_to_physical_hid_keys() {
        assert_eq!(captured("-", "French").key().code(), 0x23);
        assert_eq!(captured("eacute", "French").key().code(), 0x1f);
        assert_eq!(captured(")", "French").key().code(), 0x2d);
        assert_eq!(captured("subtract", "French").key().code(), 0x56);
        assert_eq!(captured("-", "Belgian").key().code(), 0x2d);
        assert_eq!(captured("section", "Belgian").key().code(), 0x23);
        assert_eq!(captured("-", "English (US)").key().code(), 0x2d);
        assert_eq!(captured("-", "French (Switzerland)").key().code(), 0x2d);
        assert!(KeyCombo::from_captured(CapturedKeystroke::new("shift", "French")).is_none());
    }

    #[test]
    fn rejects_missing_multiple_and_unknown_keys() {
        assert_eq!(
            "Cmd+Shift".parse::<KeyCombo>(),
            Err(KeyComboParseError::MissingKey)
        );
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
        // A bare `255` is not a TOML document, so the usage has to arrive in a
        // wire field — otherwise the parse fails on syntax before reaching the
        // usage guard.
        let Err(error) = toml::from_str::<KeyComboWire>("modifiers = 0\nkey = 255") else {
            panic!("usage 255 is not a supported HID keyboard usage and must be rejected")
        };
        assert!(
            error
                .to_string()
                .contains(&KeyboardUsageError(255).to_string()),
            "expected the usage guard to reject 255, got: {error}"
        );
        assert_eq!(
            KeyCombo::try_from(KeyComboWire {
                modifiers: 128,
                key: KeyboardUsage::try_from(0x04).expect("0x04 is a valid keyboard usage"),
            }),
            Err(KeyComboParseError::InvalidModifiers(128))
        );
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
