//! The gaming cluster of a G-series keyboard: the G-key column (HID++
//! `0x8010`), the M1–M3 bank keys (`0x8020`) and the MR key (`0x8030`).
//!
//! None of these keys is a `0x1b04` control, so like a keypad's grid keys
//! they have an identity of their own: a G-key is its printed number, and
//! because M1–M3 select one of three binding sets for the column, a G-key
//! binding is keyed by the profile it belongs to ([`GKeySlot`]) the way a
//! grid binding is keyed by its page. A press always reports the M1 slot
//! ([`ButtonId::g_key`](super::ButtonId::g_key)); the active profile decides
//! which slot's binding it runs.

use std::borrow::Cow;

use num_enum::TryFromPrimitive;
use serde::{Deserialize, Serialize};

/// One key of a G-series keyboard's G-key column, by its printed number. The
/// discriminant is the number, and bit `number - 1` of the `0x8010` state
/// event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, TryFromPrimitive)]
#[repr(u8)]
pub enum GKey {
    /// G1, the top of the column.
    G1 = 1,
    /// G2.
    G2 = 2,
    /// G3.
    G3 = 3,
    /// G4.
    G4 = 4,
    /// G5, the bottom of the column on a G913/G915.
    G5 = 5,
}

impl GKey {
    /// Every G-key OpenLogi models, top to bottom. A keyboard reporting a
    /// longer column keeps it on its onboard actions: taking the row would
    /// silence keys nothing can bind.
    pub const ALL: [Self; 5] = [Self::G1, Self::G2, Self::G3, Self::G4, Self::G5];

    /// The printed number.
    #[must_use]
    pub const fn number(self) -> u8 {
        self as u8
    }

    /// The key printed `G{number}`, or `None` past the modelled column.
    #[must_use]
    pub fn from_number(number: u8) -> Option<Self> {
        Self::try_from(number).ok()
    }

    /// The persisted name on profile M1, `KeyG1`.
    #[must_use]
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::G1 => "KeyG1",
            Self::G2 => "KeyG2",
            Self::G3 => "KeyG3",
            Self::G4 => "KeyG4",
            Self::G5 => "KeyG5",
        }
    }

    /// The inverse of [`Self::config_name`].
    #[must_use]
    pub fn from_config_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.config_name() == name)
    }

    /// The printed legend.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::G1 => "G1",
            Self::G2 => "G2",
            Self::G3 => "G3",
            Self::G4 => "G4",
            Self::G5 => "G5",
        }
    }

    /// Stable catalog key for the localized legend.
    #[must_use]
    pub const fn translation_key(self) -> &'static str {
        match self {
            Self::G1 => "keyboard.g1_key",
            Self::G2 => "keyboard.g2_key",
            Self::G3 => "keyboard.g3_key",
            Self::G4 => "keyboard.g4_key",
            Self::G5 => "keyboard.g5_key",
        }
    }
}

/// One of the three binding sets M1–M3 select for the G-key column.
///
/// Persisted by its variant name, which is also how it keys the per-device
/// profile names in `config.toml`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum GKeyProfile {
    /// The profile M1 selects, active whenever capture starts.
    #[default]
    M1,
    /// The profile M2 selects.
    M2,
    /// The profile M3 selects.
    M3,
}

impl GKeyProfile {
    /// All three profiles, in the order of their keys.
    pub const ALL: [Self; 3] = [Self::M1, Self::M2, Self::M3];

    /// The key that selects this profile.
    #[must_use]
    pub const fn mode_key(self) -> ModeKey {
        match self {
            Self::M1 => ModeKey::M1,
            Self::M2 => ModeKey::M2,
            Self::M3 => ModeKey::M3,
        }
    }

    /// The printed legend of [`Self::mode_key`].
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.mode_key().label()
    }
}

/// Separates a later profile from the key in a persisted name: `M2:KeyG1`.
const PROFILE_SEPARATOR: char = ':';

/// A G-key on one profile — what a G-key binding is keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GKeySlot {
    /// The profile the binding belongs to.
    pub profile: GKeyProfile,
    /// The physical key.
    pub key: GKey,
}

impl GKeySlot {
    /// The persisted name: the key's name on M1, and `M2:` or `M3:` + it on
    /// a later profile.
    #[must_use]
    pub fn config_name(self) -> Cow<'static, str> {
        if self.profile == GKeyProfile::M1 {
            Cow::Borrowed(self.key.config_name())
        } else {
            Cow::Owned(format!(
                "{}{PROFILE_SEPARATOR}{}",
                self.profile.label(),
                self.key.config_name()
            ))
        }
    }

    /// The inverse of [`Self::config_name`]. An M1 slot has exactly one
    /// spelling: `M1:KeyG1` is not accepted, so a config cannot bind one slot
    /// twice under two names.
    #[must_use]
    pub fn from_config_name(name: &str) -> Option<Self> {
        let Some((profile, key)) = name.split_once(PROFILE_SEPARATOR) else {
            return GKey::from_config_name(name).map(|key| Self {
                profile: GKeyProfile::M1,
                key,
            });
        };
        let profile = [GKeyProfile::M2, GKeyProfile::M3]
            .into_iter()
            .find(|candidate| candidate.label() == profile)?;
        Some(Self {
            profile,
            key: GKey::from_config_name(key)?,
        })
    }

    /// Human-readable label: the key's, plus its profile past M1.
    #[must_use]
    pub fn label(self) -> Cow<'static, str> {
        if self.profile == GKeyProfile::M1 {
            Cow::Borrowed(self.key.label())
        } else {
            Cow::Owned(format!(
                "{}, Profile {}",
                self.key.label(),
                self.profile.label()
            ))
        }
    }
}

/// A key of the row above a G-series keyboard's G-key column: the M1–M3
/// bank keys and MR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModeKey {
    /// M1.
    M1,
    /// M2.
    M2,
    /// M3.
    M3,
    /// MR, the macro-record key.
    MacroRecord,
}

impl ModeKey {
    /// Every key in physical left-to-right order.
    pub const ALL: [Self; 4] = [Self::M1, Self::M2, Self::M3, Self::MacroRecord];

    /// The profile this key selects, for M1–M3.
    #[must_use]
    pub const fn profile(self) -> Option<GKeyProfile> {
        match self {
            Self::M1 => Some(GKeyProfile::M1),
            Self::M2 => Some(GKeyProfile::M2),
            Self::M3 => Some(GKeyProfile::M3),
            Self::MacroRecord => None,
        }
    }

    /// The persisted name.
    #[must_use]
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::M1 => "KeyM1",
            Self::M2 => "KeyM2",
            Self::M3 => "KeyM3",
            Self::MacroRecord => "KeyMR",
        }
    }

    /// The inverse of [`Self::config_name`].
    #[must_use]
    pub fn from_config_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|key| key.config_name() == name)
    }

    /// The printed legend.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::M1 => "M1",
            Self::M2 => "M2",
            Self::M3 => "M3",
            Self::MacroRecord => "MR",
        }
    }

    /// Stable catalog key for the localized legend.
    #[must_use]
    pub const fn translation_key(self) -> &'static str {
        match self {
            Self::M1 => "keyboard.m1_key",
            Self::M2 => "keyboard.m2_key",
            Self::M3 => "keyboard.m3_key",
            Self::MacroRecord => "keyboard.macro_record_key",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_key_slots_have_one_spelling_per_profile() {
        for profile in GKeyProfile::ALL {
            for key in GKey::ALL {
                let slot = GKeySlot { profile, key };
                assert_eq!(GKeySlot::from_config_name(&slot.config_name()), Some(slot));
            }
        }
        assert_eq!(
            GKeySlot {
                profile: GKeyProfile::M3,
                key: GKey::G2
            }
            .config_name(),
            "M3:KeyG2"
        );
        assert_eq!(GKeySlot::from_config_name("M1:KeyG1"), None);
        assert_eq!(GKeySlot::from_config_name("M4:KeyG1"), None);
        assert_eq!(GKeySlot::from_config_name("M2:KeyG6"), None);
        assert_eq!(GKeySlot::from_config_name("KeyM1"), None);
    }

    #[test]
    fn g_key_numbers_are_the_state_bits() {
        assert_eq!(GKey::from_number(1), Some(GKey::G1));
        assert_eq!(GKey::from_number(5), Some(GKey::G5));
        assert_eq!(GKey::from_number(0), None);
        assert_eq!(GKey::from_number(6), None);
    }

    #[test]
    fn only_the_bank_keys_select_a_profile() {
        for profile in GKeyProfile::ALL {
            assert_eq!(profile.mode_key().profile(), Some(profile));
        }
        assert_eq!(ModeKey::MacroRecord.profile(), None);
        for key in ModeKey::ALL {
            assert_eq!(ModeKey::from_config_name(key.config_name()), Some(key));
        }
    }
}
