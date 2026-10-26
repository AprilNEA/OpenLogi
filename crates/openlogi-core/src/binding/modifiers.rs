//! Keyboard modifiers: their bit layout and the names a binding spells them
//! with.
//!
//! [`Modifiers`] is the set a [`super::KeyCombo`] carries beside its key.
//! [`BareModifiers`] is a non-empty set held with no key at all, the payload
//! of `Action::HoldModifier`.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

bitflags::bitflags! {
    /// A set of keyboard modifiers, possibly empty.
    ///
    /// Crosses the agent↔GUI IPC as one byte — changes to the layout require
    /// a `PROTOCOL_VERSION` bump (guarded by
    /// `openlogi-ipc/tests/wire_format.rs`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Modifiers: u8 {
        /// Command, the cross-platform primary modifier: Command on macOS,
        /// Control on Linux and Windows.
        const COMMAND = 1 << 0;
        /// Shift.
        const SHIFT = 1 << 1;
        /// Control.
        const CONTROL = 1 << 2;
        /// Option on macOS, Alt elsewhere.
        const OPTION = 1 << 3;
        /// The logo key, written `Super`, `Win`, or `Meta`: Command on macOS,
        /// the Windows key, `KEY_LEFTMETA` on Linux. Unlike
        /// [`Self::COMMAND`], it never becomes Control.
        const SUPER = 1 << 4;
    }
}

impl Modifiers {
    /// The modifier a binding token names, ignoring case, or `None` for any
    /// other token.
    pub(super) fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "cmd" | "command" => Some(Self::COMMAND),
            "super" | "win" | "meta" => Some(Self::SUPER),
            "shift" => Some(Self::SHIFT),
            "ctrl" | "control" => Some(Self::CONTROL),
            "alt" | "option" => Some(Self::OPTION),
            _ => None,
        }
    }

    /// Canonical user-facing label, or `None` for the empty set.
    pub(super) fn rendered_label(self) -> Option<String> {
        let mut parts = Vec::new();
        if self.contains(Self::COMMAND) {
            parts.push("Cmd");
        }
        if self.contains(Self::SUPER) {
            parts.push("Super");
        }
        if self.contains(Self::CONTROL) {
            parts.push("Ctrl");
        }
        if self.contains(Self::OPTION) {
            parts.push("Alt");
        }
        if self.contains(Self::SHIFT) {
            parts.push("Shift");
        }
        (!parts.is_empty()).then(|| parts.join("+"))
    }
}

/// One or more modifiers held with no ordinary key: the payload of
/// `Action::HoldModifier`.
///
/// Never empty, so a binding cannot hold nothing. Human-readable formats store
/// the canonical label (`"Ctrl+Shift"`); binary IPC stores the validated
/// [`Modifiers`] byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BareModifiers(Modifiers);

/// Why a bare-modifier binding could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BareModifiersParseError {
    /// The string was blank.
    #[error("modifier binding must not be empty")]
    Empty,
    /// The token is not a supported modifier name.
    #[error("unsupported modifier token: {0}")]
    UnknownToken(String),
    /// Serialized bits contain an unknown flag.
    #[error("unsupported modifier bits: {0:#04x}")]
    InvalidBits(u8),
    /// The serialized set is empty.
    #[error("modifier binding must contain at least one modifier")]
    NoModifiers,
}

impl BareModifiers {
    /// The held modifiers, never empty.
    #[must_use]
    pub const fn modifiers(self) -> Modifiers {
        self.0
    }

    /// Canonical user-facing label.
    #[must_use]
    pub fn rendered_label(self) -> String {
        self.0.rendered_label().unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize)]
struct BareModifiersWire(u8);

impl TryFrom<BareModifiersWire> for BareModifiers {
    type Error = BareModifiersParseError;

    fn try_from(value: BareModifiersWire) -> Result<Self, Self::Error> {
        let modifiers =
            Modifiers::from_bits(value.0).ok_or(BareModifiersParseError::InvalidBits(value.0))?;
        if modifiers.is_empty() {
            return Err(BareModifiersParseError::NoModifiers);
        }
        Ok(Self(modifiers))
    }
}

impl Serialize for BareModifiers {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.rendered_label())
        } else {
            BareModifiersWire(self.0.bits()).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for BareModifiers {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            String::deserialize(deserializer)?
                .parse()
                .map_err(de::Error::custom)
        } else {
            Self::try_from(BareModifiersWire::deserialize(deserializer)?).map_err(de::Error::custom)
        }
    }
}

impl FromStr for BareModifiers {
    type Err = BareModifiersParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(BareModifiersParseError::Empty);
        }

        let mut modifiers = Modifiers::empty();
        for raw in input.split('+') {
            let token = raw.trim();
            let Some(modifier) = Modifiers::from_token(token) else {
                return Err(BareModifiersParseError::UnknownToken(token.to_string()));
            };
            modifiers |= modifier;
        }

        Ok(Self(modifiers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_name_their_modifier_in_any_case() {
        assert_eq!(Modifiers::from_token("CMD"), Some(Modifiers::COMMAND));
        assert_eq!(Modifiers::from_token("win"), Some(Modifiers::SUPER));
        assert_eq!(Modifiers::from_token("Option"), Some(Modifiers::OPTION));
        assert_eq!(Modifiers::from_token("A"), None);
    }

    #[test]
    fn label_follows_canonical_order_and_is_none_when_empty() {
        let set = Modifiers::SHIFT | Modifiers::CONTROL | Modifiers::SUPER;
        assert_eq!(set.rendered_label().as_deref(), Some("Super+Ctrl+Shift"));
        assert_eq!(Modifiers::empty().rendered_label(), None);
    }

    #[test]
    fn bare_modifiers_parse_one_or_more_in_canonical_order() {
        let ctrl = "Ctrl".parse::<BareModifiers>().expect("Ctrl is valid");
        assert_eq!(ctrl.modifiers(), Modifiers::CONTROL);
        assert_eq!(ctrl.rendered_label(), "Ctrl");

        let combined = "Shift+Ctrl+Alt"
            .parse::<BareModifiers>()
            .expect("multi-modifier is valid");
        assert_eq!(combined.rendered_label(), "Ctrl+Alt+Shift");

        let logo = "Win+Ctrl".parse::<BareModifiers>().expect("Super is valid");
        assert_eq!(logo.modifiers(), Modifiers::SUPER | Modifiers::CONTROL);
        assert_eq!(logo.rendered_label(), "Super+Ctrl");
    }

    #[test]
    fn bare_modifiers_reject_empty_ordinary_keys_and_unknown_tokens() {
        assert_eq!(
            "".parse::<BareModifiers>(),
            Err(BareModifiersParseError::Empty)
        );
        assert!(matches!(
            "Ctrl+A".parse::<BareModifiers>(),
            Err(BareModifiersParseError::UnknownToken(t)) if t == "A"
        ));
        assert!(matches!(
            "Ctrl+Hyper".parse::<BareModifiers>(),
            Err(BareModifiersParseError::UnknownToken(t)) if t == "Hyper"
        ));
    }

    #[test]
    fn bare_modifiers_wire_rejects_unknown_bits_and_zero() {
        assert_eq!(
            BareModifiers::try_from(BareModifiersWire(128)),
            Err(BareModifiersParseError::InvalidBits(128))
        );
        assert_eq!(
            BareModifiers::try_from(BareModifiersWire(0)),
            Err(BareModifiersParseError::NoModifiers)
        );
    }

    #[test]
    fn bare_modifiers_toml_uses_the_canonical_text_label() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct Wrapper {
            hold: BareModifiers,
        }
        let hold = "Ctrl+Shift".parse::<BareModifiers>().expect("valid");
        let wrapper = Wrapper { hold };
        let encoded = toml::to_string(&wrapper).expect("serialization");
        assert_eq!(encoded, "hold = \"Ctrl+Shift\"\n");
        assert_eq!(toml::from_str::<Wrapper>(&encoded), Ok(wrapper));
    }
}
