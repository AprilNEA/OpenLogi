//! Domain types for the `OnboardProfiles` feature (`0x8100`).

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::protocol::v20::Hidpp20Error;

/// Who drives the device's settings.
///
/// The firmware falls back to [`Self::Onboard`] whenever it restarts — power
/// cycle, reconnect, wake — so host mode only lasts while software keeps
/// asserting it. The wire's third value, `0` ("leave unchanged"), is a
/// write-only sentinel and has no variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum OnboardMode {
    /// The firmware runs the active profile stored in its own memory.
    Onboard = 1,
    /// Software on the host drives DPI, lighting and buttons live.
    Host = 2,
}

/// The memory layout a device declares. Only the G402 layout — a directory
/// sector followed by one sector per profile — is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum MemoryModel {
    /// Directory at sector `0x0000`, user profiles from `0x0001`, read-only
    /// factory profiles from `0x0101`.
    G402 = 1,
}

/// The byte layout of one profile sector, named after the mouse that
/// introduced it. Every known format shares the fields this crate reads and
/// writes; the later ones only add fields after them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum ProfileFormat {
    /// The original layout.
    G402 = 1,
    /// Adds the two LED zones at offset `0xd0`.
    G303 = 2,
    /// G900 layout.
    G900 = 3,
    /// G915 layout.
    G915 = 4,
    /// G502 X layout, which adds a custom-animation index after the LEDs.
    G502X = 5,
}

/// The macro encoding a device declares. Only one is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum MacroFormat {
    /// The G402 macro encoding.
    G402 = 1,
}

/// What `getOnboardProfilesDescription` reports about a device's memory.
///
/// The layout ids stay raw here: a device may declare a layout this crate
/// cannot decode, and that is a fact to report, not a failed read. The
/// `*_format` accessors type them for the code that must refuse an unknown
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OnboardProfilesInfo {
    /// Raw memory-model id; see [`Self::memory_model`].
    pub memory_model_id: u8,
    /// Raw profile-format id; see [`Self::profile_format`].
    pub profile_format_id: u8,
    /// Raw macro-format id; see [`Self::macro_format`].
    pub macro_format_id: u8,
    /// How many user profiles the device can hold.
    pub profile_count: u8,
    /// How many read-only factory profiles it ships with.
    pub rom_profile_count: u8,
    /// How many buttons a profile binds.
    pub button_count: u8,
    /// How many writable sectors the memory has.
    pub sector_count: u8,
    /// The size of one sector in bytes, its trailing CRC included.
    pub sector_size: u16,
    /// Bit field: G-shift and DPI-shift support.
    pub mechanical_layout: u8,
    /// Bit field: corded and wireless operation.
    pub various_info: u8,
}

impl OnboardProfilesInfo {
    /// Decode the `getOnboardProfilesDescription` reply.
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            memory_model_id: payload[0],
            profile_format_id: payload[1],
            macro_format_id: payload[2],
            profile_count: payload[3],
            rom_profile_count: payload[4],
            button_count: payload[5],
            sector_count: payload[6],
            sector_size: u16::from_be_bytes([payload[7], payload[8]]),
            mechanical_layout: payload[9],
            various_info: payload[10],
        }
    }

    /// The declared memory model, or `UnsupportedResponse` for one this
    /// crate does not know.
    pub fn memory_model(&self) -> Result<MemoryModel, Hidpp20Error> {
        MemoryModel::try_from(self.memory_model_id).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// The declared profile format, or `UnsupportedResponse` for one this
    /// crate does not know.
    pub fn profile_format(&self) -> Result<ProfileFormat, Hidpp20Error> {
        ProfileFormat::try_from(self.profile_format_id)
            .map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// The declared macro format, or `UnsupportedResponse` for one this
    /// crate does not know.
    pub fn macro_format(&self) -> Result<MacroFormat, Hidpp20Error> {
        MacroFormat::try_from(self.macro_format_id).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}

/// The address of one memory sector.
///
/// Sector `0x0000` is the user profile directory, `0x0001..` the user
/// profiles, and `0x0100 | n` the read-only factory profile `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SectorAddress(u16);

impl SectorAddress {
    /// The user profile directory.
    pub const DIRECTORY: Self = Self(0x0000);

    /// Bit set in every read-only factory sector address.
    const ROM: u16 = 0x0100;

    /// The sector at raw address `raw`.
    #[must_use]
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// User profile `number` (1-based).
    #[must_use]
    pub const fn user_profile(number: u8) -> Self {
        Self(number as u16)
    }

    /// The raw address.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// Whether this is a read-only factory profile.
    #[must_use]
    pub const fn is_rom(self) -> bool {
        self.0 & Self::ROM != 0
    }

    /// The 1-based profile number within its bank (user or factory).
    #[must_use]
    pub fn profile_number(self) -> u8 {
        self.0.to_be_bytes()[1]
    }
}
