//! Implements the `OnboardProfiles` feature (ID `0x8100`) that controls a
//! gaming device's onboard profile memory.
//!
//! In onboard mode the device applies a profile stored in its own flash and
//! ignores most host software settings; in host mode the host drives the
//! device. This implementation covers reading the memory description, getting
//! and setting the mode and the active profile, and reading flash sectors —
//! enough to parse the profile directory. The flash *write* session
//! (`memoryAddrWrite` / `memoryWrite` / `memoryWriteEnd`, functions 6–8) is
//! deliberately not implemented: OpenLogi does not edit onboard profiles.
//!
//! The official `x8100` specification is not public; the protocol facts here
//! are reverse-engineered, cross-checked against libratbag (`hidpp20.c`) and
//! Solaar (`hidpp20.py`). All multi-byte fields are big-endian.

pub mod types;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use types::{
    DIRECTORY_ENTRY_LEN, DIRECTORY_SECTOR, OnboardMode, ProfileDirectoryEntry, ProfilesDescription,
    ROM_SECTOR_FLAG,
};

use self::types::{DIRECTORY_END, be16, parse_directory};
use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements the `OnboardProfiles` / `0x8100` feature.
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for OnboardProfilesFeature {
    const ID: u16 = 0x8100;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for OnboardProfilesFeature {}

impl OnboardProfilesFeature {
    /// Retrieves the description of the device's profile memory.
    pub async fn get_description(&self) -> Result<ProfilesDescription, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();

        Ok(ProfilesDescription::from_payload(&payload))
    }

    /// Sets whether the device applies its onboard profile or host settings.
    pub async fn set_onboard_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [mode.into(), 0, 0]).await?;

        Ok(())
    }

    /// Retrieves whether the device applies its onboard profile or host
    /// settings.
    pub async fn get_onboard_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();

        OnboardMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Sets the active profile by its flash sector.
    ///
    /// User profiles live in sectors `0x0001..`; ROM profiles carry
    /// [`ROM_SECTOR_FLAG`](types::ROM_SECTOR_FLAG).
    ///
    /// Only legal in [`OnboardMode::Onboard`]: in host mode the firmware
    /// rejects this with an invalid-argument error (observed on a G502 X
    /// LIGHTSPEED; the official spec is not public).
    pub async fn set_current_profile(&self, sector: u16) -> Result<(), Hidpp20Error> {
        let [hi, lo] = sector.to_be_bytes();
        self.endpoint.call(3, [hi, lo, 0]).await?;

        Ok(())
    }

    /// Retrieves the sector of the active profile.
    ///
    /// Devices report `0x0000` when no profile has been activated yet.
    pub async fn get_current_profile(&self) -> Result<u16, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();

        Ok(be16(&payload, 0))
    }

    /// Reads 16 bytes of flash at `offset` of `sector`.
    ///
    /// The firmware rejects reads past `sector_size - 16` with an
    /// invalid-argument error, so a full-sector read must fetch the final
    /// partial chunk from `sector_size - 16`.
    pub async fn memory_read(&self, sector: u16, offset: u16) -> Result<[u8; 16], Hidpp20Error> {
        let mut args = [0; 16];
        args[..2].copy_from_slice(&sector.to_be_bytes());
        args[2..4].copy_from_slice(&offset.to_be_bytes());

        Ok(self.endpoint.call_long(5, args).await?.extend_payload())
    }

    /// Reads and parses the profile directory from sector
    /// [`DIRECTORY_SECTOR`](types::DIRECTORY_SECTOR).
    ///
    /// `profile_count` (from [`Self::get_description`]) bounds the number of
    /// entries; reading stops early at the directory terminator.
    pub async fn read_profile_directory(
        &self,
        profile_count: u8,
    ) -> Result<Vec<ProfileDirectoryEntry>, Hidpp20Error> {
        let max_entries = usize::from(profile_count);
        // Room for every entry plus the terminator entry.
        let needed = (max_entries + 1) * DIRECTORY_ENTRY_LEN;

        let mut bytes: Vec<u8> = Vec::with_capacity(needed.next_multiple_of(16));
        while bytes.len() < needed && !contains_terminator(&bytes) {
            let offset =
                u16::try_from(bytes.len()).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
            bytes.extend_from_slice(&self.memory_read(DIRECTORY_SECTOR, offset).await?);
        }

        parse_directory(&bytes, max_entries)
    }
}

/// Whether any complete directory entry in `bytes` is the terminator.
fn contains_terminator(bytes: &[u8]) -> bool {
    bytes
        .chunks_exact(DIRECTORY_ENTRY_LEN)
        .any(|entry| be16(entry, 0) == DIRECTORY_END)
}
