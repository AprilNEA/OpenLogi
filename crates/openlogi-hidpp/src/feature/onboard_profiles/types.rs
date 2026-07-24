//! Domain types for the `OnboardProfiles` feature (`0x8100`).

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::protocol::v20::Hidpp20Error;

/// Sector holding the profile directory.
pub const DIRECTORY_SECTOR: u16 = 0x0000;

/// Bit set in sector numbers referring to ROM (factory) profiles rather than
/// writable user profiles.
pub const ROM_SECTOR_FLAG: u16 = 0x0100;

/// Sector value terminating the profile directory. Also what erased flash
/// (`0xFF` bytes) reads back as, so an empty directory parses as no entries.
pub const DIRECTORY_END: u16 = 0xffff;

/// Size of one profile-directory entry in bytes.
pub const DIRECTORY_ENTRY_LEN: usize = 4;

/// Reads a big-endian `u16` at `offset` of a payload.
pub(super) fn be16(payload: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([payload[offset], payload[offset + 1]])
}

/// Whether profile settings come from onboard flash or host software.
///
/// The wire encoding also defines `0x00` as "no change" for set requests; it is
/// never a valid mode report, so it is deliberately not representable here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum OnboardMode {
    /// The device applies the profile stored in its onboard memory.
    Onboard = 1,
    /// The device takes its settings from host software.
    Host = 2,
}

/// The `getProfilesDescription` response describing the device's profile
/// memory.
///
/// Field order matches the wire layout as implemented by libratbag
/// (`hidpp20_onboard_profiles_info`); the official `x8100` specification is not
/// public. The format ids are kept raw — they are informational and never
/// branched on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ProfilesDescription {
    /// Memory model identifier.
    pub memory_model_id: u8,

    /// Profile format identifier.
    pub profile_format_id: u8,

    /// Macro format identifier.
    pub macro_format_id: u8,

    /// Number of writable user profiles.
    pub profile_count: u8,

    /// Number of out-of-box (ROM) profiles.
    pub profile_count_oob: u8,

    /// Number of physical buttons covered by a profile.
    pub button_count: u8,

    /// Number of writable flash sectors.
    pub sector_count: u8,

    /// Size of one flash sector in bytes.
    pub sector_size: u16,

    /// Mechanical layout descriptor (raw).
    pub mechanical_layout: u8,

    /// Additional device info (raw).
    pub various_info: u8,
}

impl ProfilesDescription {
    /// Parses a description from a `getProfilesDescription` response payload.
    pub(super) fn from_payload(payload: &[u8; 16]) -> Self {
        Self {
            memory_model_id: payload[0],
            profile_format_id: payload[1],
            macro_format_id: payload[2],
            profile_count: payload[3],
            profile_count_oob: payload[4],
            button_count: payload[5],
            sector_count: payload[6],
            sector_size: be16(payload, 7),
            mechanical_layout: payload[9],
            various_info: payload[10],
        }
    }
}

/// One entry of the profile directory in sector
/// [`DIRECTORY_SECTOR`](self::DIRECTORY_SECTOR).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ProfileDirectoryEntry {
    /// The flash sector holding the profile.
    pub sector: u16,

    /// Whether the profile is enabled.
    pub enabled: bool,
}

/// Parses profile-directory entries out of accumulated sector bytes.
///
/// Entries are 4 bytes each — `sector` (big-endian `u16`), an enabled byte and
/// a reserved byte — and the directory ends at a [`DIRECTORY_END`] sector or
/// after `max_entries` entries, whichever comes first. Running out of bytes
/// before either bound, or an enabled byte other than `0`/`1`, is an
/// [`UnsupportedResponse`](Hidpp20Error::UnsupportedResponse).
pub(super) fn parse_directory(
    bytes: &[u8],
    max_entries: usize,
) -> Result<Vec<ProfileDirectoryEntry>, Hidpp20Error> {
    let mut entries = Vec::new();
    let mut offset = 0;

    while entries.len() < max_entries {
        let Some(entry) = bytes.get(offset..offset + DIRECTORY_ENTRY_LEN) else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };

        let sector = be16(entry, 0);
        if sector == DIRECTORY_END {
            break;
        }

        let enabled = match entry[2] {
            0 => false,
            1 => true,
            _ => return Err(Hidpp20Error::UnsupportedResponse),
        };

        entries.push(ProfileDirectoryEntry { sector, enabled });
        offset += DIRECTORY_ENTRY_LEN;
    }

    Ok(entries)
}
