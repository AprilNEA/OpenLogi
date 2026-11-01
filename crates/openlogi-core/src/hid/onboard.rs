//! Gaming-mouse onboard memory. Wire types: field order matters.

use std::fmt;
use std::num::NonZeroU8;

use serde::{Deserialize, Serialize};

use super::{Dpi, ReportRate};
use crate::binding::FlashWrite;

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

/// The pointer settings one flash profile stores: what the mouse starts in
/// when it selects the profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashProfileSettings {
    /// The DPI modes the mouse's DPI buttons step through, in order; `None`
    /// for a mode the profile leaves off.
    pub dpi_modes: Vec<Option<Dpi>>,
    /// The mode the profile starts in, counted from 0; `None` when the
    /// profile's byte names no mode it has (real G700 profiles carry
    /// `0x82` there).
    pub default_mode: Option<u8>,
    /// The report rate the profile plays at; `None` for an interval byte
    /// that names no rate.
    pub report_rate: Option<ReportRate>,
}

impl FlashProfileSettings {
    /// The DPI of every mode the profile has on, in mode order.
    #[must_use]
    pub fn enabled_dpi_modes(&self) -> Vec<Dpi> {
        self.dpi_modes.iter().flatten().copied().collect()
    }
}

/// A profile stored in flash that the directory no longer lists, which
/// [`FlashProfileEdit::Enable`] lists again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisabledProfile {
    /// The memory page it is stored on: its identity while unlisted.
    pub page: u8,
    /// Its stored name, as [`OnboardProfile::name`].
    pub name: Option<String>,
}

/// The onboard profiles of a mouse whose flash is the source of truth for
/// its settings (the G700): read from the mouse, never from configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashProfiles {
    /// The directory's slots, every one of them, enabled or not, and the
    /// profile the mouse plays — `None` while it plays its factory profile.
    /// The mode is always [`OnboardMode::Onboard`].
    pub onboard: OnboardState,
    /// The settings of the profile the mouse plays; `None` while it plays
    /// its factory profile.
    pub active: Option<FlashProfileSettings>,
    /// Profiles that can be listed again, in page order.
    pub disabled: Vec<DisabledProfile>,
    /// Whether an erased page is left for a new profile.
    pub can_create: bool,
    /// Where the latest queued flash write stands — profile edits, DPI and
    /// report-rate saves and button bindings share it.
    pub write: FlashWrite,
    /// The flash page each listed profile is stored on, in directory order:
    /// the identity a profile's OpenLogi configuration is kept under
    /// ([`Config::flash_profile_config`](crate::config::Config::flash_profile_config)).
    pub pages: Vec<u8>,
}

impl FlashProfiles {
    /// The page `profile` is stored on, if it is listed.
    #[must_use]
    pub fn page_of(&self, profile: ProfileIndex) -> Option<u8> {
        self.pages
            .get(usize::from(profile.get().get() - 1))
            .copied()
    }
}

impl FlashProfiles {
    /// The number of enabled profiles.
    #[must_use]
    pub fn enabled_count(&self) -> usize {
        self.onboard.profiles.iter().filter(|p| p.enabled).count()
    }

    /// Whether `profile` may be disabled: it is enabled and not the last one
    /// that is, since a mouse with no profile listed plays only its factory
    /// profile.
    #[must_use]
    pub fn can_disable(&self, profile: ProfileIndex) -> bool {
        self.enabled_count() > 1
            && self
                .onboard
                .profiles
                .iter()
                .any(|p| p.index == profile && p.enabled)
    }

    /// Whether a slot is free for a new or re-enabled profile.
    #[must_use]
    pub fn has_free_slot(&self) -> bool {
        self.onboard.profiles.iter().any(|p| !p.enabled)
    }
}

/// How many UTF-16 units a flash profile's name holds (the G700's 23).
pub const FLASH_PROFILE_NAME_UNITS: usize = 23;

/// A change to a flash mouse's onboard profiles, asked of the agent.
///
/// [`Self::Activate`] is a register write the agent makes at once; every
/// other edit rewrites flash, so the agent queues it with the mouse's other
/// flash writes and runs it to its end (see [`FlashProfiles::write`]).
/// Indexes name directory slots as [`OnboardState::profiles`] numbers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlashProfileEdit {
    /// Play this enabled profile now.
    Activate(ProfileIndex),
    /// List the profile stored on this page again, in the first free slot.
    Enable {
        /// Its page, as [`DisabledProfile::page`].
        page: u8,
    },
    /// Stop listing this profile. Its page keeps its content, so it can be
    /// enabled again; the slots after it move up one. Refused for the last
    /// enabled profile.
    Disable(ProfileIndex),
    /// Rename this profile.
    Rename {
        /// The profile.
        profile: ProfileIndex,
        /// Its new name.
        name: String,
    },
    /// Store a copy of this profile on an erased page, listed in the first
    /// free slot.
    Duplicate(ProfileIndex),
    /// Replace the DPI modes of the profile the mouse plays with these, in
    /// order; modes past the list are turned off.
    DpiModes(Vec<Dpi>),
}
