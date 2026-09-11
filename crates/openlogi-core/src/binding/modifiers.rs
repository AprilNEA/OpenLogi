//! Bare-modifier set for `Action::HoldModifier`.
//!
//! Sibling of [`super::KeyCombo`]: same modifier bit layout, no ordinary key.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use super::key_combo::{
    ALL_MODIFIERS, MOD_COMMAND, MOD_CONTROL, MOD_OPTION, MOD_SHIFT, parse_modifier,
};

/// One or more held modifier keys, with no ordinary key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Modifiers(u8);

/// Why a user-entered modifier string could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ModifiersParseError {
    /// The string was blank.
    #[error("modifier binding must not be empty")]
    Empty,
    /// The token is not a supported modifier name.
    #[error("unsupported modifier token: {0}")]
    UnknownToken(String),
    /// Serialized bits contain an unknown flag.
    #[error("unsupported modifier bits: {0:#04x}")]
    InvalidBits(u8),
    /// The modifier set is empty.
    #[error("modifier binding must contain at least one modifier")]
    NoModifiers,
}

impl Modifiers {
    /// Whether Command/Meta is included.
    #[must_use]
    pub const fn has_command(self) -> bool {
        self.0 & MOD_COMMAND != 0
    }

    /// Whether Shift is included.
    #[must_use]
    pub const fn has_shift(self) -> bool {
        self.0 & MOD_SHIFT != 0
    }

    /// Whether Control is included.
    #[must_use]
    pub const fn has_control(self) -> bool {
        self.0 & MOD_CONTROL != 0
    }

    /// Whether Option/Alt is included.
    #[must_use]
    pub const fn has_option(self) -> bool {
        self.0 & MOD_OPTION != 0
    }

    /// Canonical user-facing label.
    #[must_use]
    pub fn rendered_label(self) -> String {
        let mut parts = Vec::new();
        if self.has_command() {
            parts.push("Cmd");
        }
        if self.has_control() {
            parts.push("Ctrl");
        }
        if self.has_option() {
            parts.push("Alt");
        }
        if self.has_shift() {
            parts.push("Shift");
        }
        parts.join("+")
    }
}

#[derive(Serialize, Deserialize)]
struct ModifiersWire(u8);

impl TryFrom<ModifiersWire> for Modifiers {
    type Error = ModifiersParseError;

    fn try_from(value: ModifiersWire) -> Result<Self, Self::Error> {
        if value.0 & !ALL_MODIFIERS != 0 {
            return Err(ModifiersParseError::InvalidBits(value.0));
        }
        if value.0 == 0 {
            return Err(ModifiersParseError::NoModifiers);
        }
        Ok(Self(value.0))
    }
}

impl Serialize for Modifiers {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.rendered_label())
        } else {
            ModifiersWire(self.0).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Modifiers {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            String::deserialize(deserializer)?
                .parse()
                .map_err(de::Error::custom)
        } else {
            Self::try_from(ModifiersWire::deserialize(deserializer)?).map_err(de::Error::custom)
        }
    }
}

impl FromStr for Modifiers {
    type Err = ModifiersParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ModifiersParseError::Empty);
        }

        let mut bits = 0u8;
        for raw in input.split('+') {
            let token = raw.trim();
            let Some(bit) = parse_modifier(token) else {
                return Err(ModifiersParseError::UnknownToken(token.to_string()));
            };
            bits |= bit;
        }

        Ok(Self(bits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_and_multiple_modifiers_in_canonical_order() {
        let ctrl = "Ctrl".parse::<Modifiers>().expect("Ctrl is valid");
        assert!(ctrl.has_control());
        assert_eq!(ctrl.rendered_label(), "Ctrl");

        let combined = "Shift+Ctrl+Alt"
            .parse::<Modifiers>()
            .expect("multi-modifier is valid");
        assert!(combined.has_shift() && combined.has_control() && combined.has_option());
        assert_eq!(combined.rendered_label(), "Ctrl+Alt+Shift");
    }

    #[test]
    fn rejects_empty_ordinary_keys_and_unknown_tokens() {
        assert_eq!("".parse::<Modifiers>(), Err(ModifiersParseError::Empty));
        assert!(matches!(
            "Ctrl+A".parse::<Modifiers>(),
            Err(ModifiersParseError::UnknownToken(t)) if t == "A"
        ));
        assert!(matches!(
            "Ctrl+Hyper".parse::<Modifiers>(),
            Err(ModifiersParseError::UnknownToken(t)) if t == "Hyper"
        ));
    }

    #[test]
    fn wire_rejects_unknown_bits_and_zero() {
        assert_eq!(
            Modifiers::try_from(ModifiersWire(128)),
            Err(ModifiersParseError::InvalidBits(128))
        );
        assert_eq!(
            Modifiers::try_from(ModifiersWire(0)),
            Err(ModifiersParseError::NoModifiers)
        );
    }

    #[test]
    fn toml_uses_the_canonical_text_label() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct Wrapper {
            hold: Modifiers,
        }
        let hold = "Ctrl+Shift".parse::<Modifiers>().expect("valid");
        let wrapper = Wrapper { hold };
        let encoded = toml::to_string(&wrapper).expect("serialization");
        assert_eq!(encoded, "hold = \"Ctrl+Shift\"\n");
        assert_eq!(toml::from_str::<Wrapper>(&encoded), Ok(wrapper));
    }
}
