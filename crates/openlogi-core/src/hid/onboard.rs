//! Gaming-mouse onboard memory. Wire types: field order matters.

use std::fmt;
use std::num::NonZeroU8;

use serde::{Deserialize, Serialize};

/// A 1-based onboard profile number. Config and wire carry the plain number;
/// the firmware has no profile 0, so it never deserializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileIndex(NonZeroU8);

impl ProfileIndex {
    /// The profile numbered `index`; `None` for 0.
    #[must_use]
    pub const fn new(index: u8) -> Option<Self> {
        match NonZeroU8::new(index) {
            Some(index) => Some(Self(index)),
            None => None,
        }
    }

    /// The number as the firmware takes it.
    #[must_use]
    pub const fn get(self) -> NonZeroU8 {
        self.0
    }
}

impl From<NonZeroU8> for ProfileIndex {
    fn from(index: NonZeroU8) -> Self {
        Self(index)
    }
}

impl fmt::Display for ProfileIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Who owns a gaming mouse's buttons, DPI levels and report rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OnboardMode {
    /// A stored profile.
    Onboard,
    /// OpenLogi.
    Host,
}

/// One onboard profile slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfile {
    /// Slot number.
    pub index: ProfileIndex,
    /// Stored name.
    pub name: Option<String>,
    /// Whether it can be made active.
    pub enabled: bool,
}

/// Onboard memory as read from the mouse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardState {
    /// Current owner.
    pub mode: OnboardMode,
    /// Active profile; `None` in host mode.
    pub active_profile: Option<ProfileIndex>,
    /// Profile slots.
    pub profiles: Vec<OnboardProfile>,
}
