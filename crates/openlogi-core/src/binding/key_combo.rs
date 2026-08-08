//! Parsing for user-entered keyboard shortcuts.

use std::str::FromStr;

use thiserror::Error;

use super::KeyCombo;

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
        let Some((key_code, key_label)) = key else {
            return Err(KeyComboParseError::MissingKey);
        };

        Ok(Self {
            modifiers,
            key_code,
            display: display_shortcut(modifiers, &key_label),
        })
    }
}

fn parse_modifier(token: &str) -> Option<u8> {
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "win" => Some(KeyCombo::MOD_CMD),
        "shift" => Some(KeyCombo::MOD_SHIFT),
        "ctrl" | "control" => Some(KeyCombo::MOD_CTRL),
        "alt" | "option" => Some(KeyCombo::MOD_OPTION),
        _ => None,
    }
}

fn parse_key(token: &str) -> Result<(u16, String), KeyComboParseError> {
    let lowercase = token.to_ascii_lowercase();
    if lowercase.len() == 1 {
        let character = lowercase.chars().next().unwrap_or_default();
        if let Some(key_code) = letter_key_code(character) {
            return Ok((key_code, character.to_ascii_uppercase().to_string()));
        }
        if let Some(key_code) = digit_key_code(character) {
            return Ok((key_code, character.to_string()));
        }
        if let Some(key_code) = punctuation_key_code(character) {
            return Ok((key_code, character.to_string()));
        }
    }
    if let Some(number) = lowercase
        .strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        && let Some(key_code) = function_key_code(number)
    {
        return Ok((key_code, format!("F{number}")));
    }

    let named = match lowercase.as_str() {
        "enter" | "return" => (0x24, "Enter"),
        "tab" => (0x30, "Tab"),
        "space" => (0x31, "Space"),
        "backspace" => (0x33, "Backspace"),
        "escape" | "esc" => (0x35, "Escape"),
        "home" => (0x73, "Home"),
        "end" => (0x77, "End"),
        "pageup" | "page-up" => (0x74, "PageUp"),
        "pagedown" | "page-down" => (0x79, "PageDown"),
        "delete" => (0x75, "Delete"),
        "left" => (0x7B, "Left"),
        "right" => (0x7C, "Right"),
        "down" => (0x7D, "Down"),
        "up" => (0x7E, "Up"),
        _ => return Err(KeyComboParseError::UnknownToken(token.to_string())),
    };
    Ok((named.0, named.1.to_string()))
}

fn display_shortcut(modifiers: u8, key: &str) -> String {
    let mut parts = Vec::new();
    if modifiers & KeyCombo::MOD_CMD != 0 {
        parts.push("Cmd");
    }
    if modifiers & KeyCombo::MOD_CTRL != 0 {
        parts.push("Ctrl");
    }
    if modifiers & KeyCombo::MOD_OPTION != 0 {
        parts.push("Alt");
    }
    if modifiers & KeyCombo::MOD_SHIFT != 0 {
        parts.push("Shift");
    }
    parts.push(key);
    parts.join("+")
}

fn letter_key_code(key: char) -> Option<u16> {
    Some(match key {
        'a' => 0x00,
        'b' => 0x0B,
        'c' => 0x08,
        'd' => 0x02,
        'e' => 0x0E,
        'f' => 0x03,
        'g' => 0x05,
        'h' => 0x04,
        'i' => 0x22,
        'j' => 0x26,
        'k' => 0x28,
        'l' => 0x25,
        'm' => 0x2E,
        'n' => 0x2D,
        'o' => 0x1F,
        'p' => 0x23,
        'q' => 0x0C,
        'r' => 0x0F,
        's' => 0x01,
        't' => 0x11,
        'u' => 0x20,
        'v' => 0x09,
        'w' => 0x0D,
        'x' => 0x07,
        'y' => 0x10,
        'z' => 0x06,
        _ => return None,
    })
}

fn digit_key_code(key: char) -> Option<u16> {
    Some(match key {
        '0' => 0x1D,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '5' => 0x17,
        '6' => 0x16,
        '7' => 0x1A,
        '8' => 0x1C,
        '9' => 0x19,
        _ => return None,
    })
}

fn punctuation_key_code(key: char) -> Option<u16> {
    Some(match key {
        '-' => 0x1B,
        '=' => 0x18,
        '[' => 0x21,
        ']' => 0x1E,
        '\\' => 0x2A,
        ';' => 0x29,
        '\'' => 0x27,
        ',' => 0x2B,
        '.' => 0x2F,
        '/' => 0x2C,
        '`' => 0x32,
        _ => return None,
    })
}

fn function_key_code(number: u8) -> Option<u16> {
    const CODES: [u16; 20] = [
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71,
        0x6A, 0x40, 0x4F, 0x50, 0x5A,
    ];
    number
        .checked_sub(1)
        .and_then(|index| CODES.get(usize::from(index)).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_letters_and_navigation_keys() {
        let combo = "Cmd+Shift+P"
            .parse::<KeyCombo>()
            .unwrap_or_else(|error| panic!("valid shortcut failed: {error}"));
        assert_eq!(combo.modifiers, KeyCombo::MOD_CMD | KeyCombo::MOD_SHIFT);
        assert_eq!(combo.key_code, 0x23);
        assert_eq!(combo.display, "Cmd+Shift+P");

        let combo = "Ctrl+Alt+Left"
            .parse::<KeyCombo>()
            .unwrap_or_else(|error| panic!("valid shortcut failed: {error}"));
        assert_eq!(combo.key_code, 0x7B);
        assert_eq!(combo.display, "Ctrl+Alt+Left");
    }

    #[test]
    fn parses_a_despite_its_zero_virtual_key_code() {
        let combo = "Cmd+A"
            .parse::<KeyCombo>()
            .unwrap_or_else(|error| panic!("valid shortcut failed: {error}"));
        assert_eq!(combo.key_code, 0x00);
        assert_eq!(combo.display, "Cmd+A");
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
}
