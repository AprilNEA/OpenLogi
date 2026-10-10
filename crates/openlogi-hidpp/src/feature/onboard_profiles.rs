//! Implements the `OnboardProfiles` feature (ID `0x8100`).
//!
//! Reverse-engineered (libratbag), checked on a G502 LIGHTSPEED. Never writes flash.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

const FN_GET_INFO: u8 = 0;
const FN_SET_MODE: u8 = 1;
const FN_GET_MODE: u8 = 2;
const FN_SET_CURRENT_PROFILE: u8 = 3;
const FN_GET_CURRENT_PROFILE: u8 = 4;
const FN_MEMORY_READ: u8 = 5;

const USER_DIRECTORY_SECTOR: u16 = 0x0000;
const ROM_DIRECTORY_SECTOR: u16 = 0x0100;
const READ_CHUNK: u16 = 16;
// UTF-16LE name in profile formats 2–5.
const NAME_OFFSET: u16 = 0xa0;
const NAME_LEN: usize = 48;
// What G HUB writes into an unnamed profile.
const UNNAMED_PLACEHOLDER: &str = "PROFILE_NAME_DEFAULT";

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

/// Who owns buttons, DPI levels and report rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum OnboardMode {
    /// The active profile.
    Onboard = 1,
    /// The host; report rate is only writable here.
    Host = 2,
}

/// The static description `getInfo` returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OnboardProfilesInfo {
    /// Memory model identifier.
    pub memory_model: u8,
    /// Layout version of a stored profile.
    pub profile_format: u8,
    /// Layout version of a stored macro.
    pub macro_format: u8,
    /// Number of user profile slots.
    pub profile_count: u8,
    /// Number of factory profiles.
    pub profile_count_oob: u8,
    /// Number of buttons a profile binds.
    pub button_count: u8,
    /// Number of writable sectors.
    pub sector_count: u8,
    /// Size of one sector in bytes.
    pub sector_size: u16,
}

/// One entry of the profile directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProfileEntry {
    /// 1-based index.
    pub index: u8,
    /// Flash sector.
    pub sector: u16,
    /// Whether it can be made current.
    pub enabled: bool,
}

impl OnboardProfilesFeature {
    /// Reads the feature's static description.
    pub async fn get_info(&self) -> Result<OnboardProfilesInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_INFO, [0; 3])
            .await?
            .extend_payload();
        Ok(info_from_payload(&payload))
    }

    /// Reads the mode.
    pub async fn get_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_MODE, [0; 3])
            .await?
            .extend_payload();
        OnboardMode::try_from(payload[0]).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Sets the mode until the next power cycle.
    pub async fn set_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(FN_SET_MODE, [u8::from(mode), 0, 0])
            .await?;
        Ok(())
    }

    /// The active profile's 1-based index, or `None` in host mode.
    pub async fn get_current_profile(&self) -> Result<Option<u8>, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_CURRENT_PROFILE, [0; 3])
            .await?
            .extend_payload();
        Ok((payload[1] != 0).then_some(payload[1]))
    }

    /// Makes the enabled profile `index` (1-based) current.
    pub async fn set_current_profile(&self, index: u8) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(FN_SET_CURRENT_PROFILE, [0, index, 0])
            .await?;
        Ok(())
    }

    /// Reads 16 bytes of profile memory.
    pub async fn read_memory(&self, sector: u16, offset: u16) -> Result<[u8; 16], Hidpp20Error> {
        let [sector_hi, sector_lo] = sector.to_be_bytes();
        let [offset_hi, offset_lo] = offset.to_be_bytes();
        let mut args = [0; 16];
        args[..4].copy_from_slice(&[sector_hi, sector_lo, offset_hi, offset_lo]);
        Ok(self
            .endpoint
            .call_long(FN_MEMORY_READ, args)
            .await?
            .extend_payload())
    }

    /// Reads the user directory, or the factory one if blank.
    pub async fn read_directory(
        &self,
        info: &OnboardProfilesInfo,
    ) -> Result<Vec<ProfileEntry>, Hidpp20Error> {
        let user = self
            .read_directory_sector(USER_DIRECTORY_SECTOR, info)
            .await?;
        if !user.is_empty() {
            return Ok(user);
        }
        self.read_directory_sector(ROM_DIRECTORY_SECTOR, info).await
    }

    async fn read_directory_sector(
        &self,
        sector: u16,
        info: &OnboardProfilesInfo,
    ) -> Result<Vec<ProfileEntry>, Hidpp20Error> {
        let slots = usize::from(info.profile_count.max(info.profile_count_oob));
        let mut bytes = Vec::with_capacity(slots * 4 + 16);
        let mut offset = 0;
        while bytes.len() < slots * 4 {
            bytes.extend_from_slice(&self.read_memory(sector, offset).await?);
            offset += READ_CHUNK;
        }
        Ok(directory_from_bytes(&bytes, slots))
    }

    /// The name stored in `sector`, if any.
    pub async fn read_profile_name(
        &self,
        info: &OnboardProfilesInfo,
        sector: u16,
    ) -> Result<Option<String>, Hidpp20Error> {
        if !(2..=5).contains(&info.profile_format) {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(NAME_LEN);
        let mut offset = NAME_OFFSET;
        while bytes.len() < NAME_LEN {
            bytes.extend_from_slice(&self.read_memory(sector, offset).await?);
            offset += READ_CHUNK;
        }
        Ok(name_from_bytes(&bytes[..NAME_LEN]))
    }
}

fn info_from_payload(payload: &[u8; 16]) -> OnboardProfilesInfo {
    OnboardProfilesInfo {
        memory_model: payload[0],
        profile_format: payload[1],
        macro_format: payload[2],
        profile_count: payload[3],
        profile_count_oob: payload[4],
        button_count: payload[5],
        sector_count: payload[6],
        sector_size: u16::from_be_bytes([payload[7], payload[8]]),
    }
}

// Entries are `sector` (BE), `enabled`, reserved; `0xffff` ends the list.
fn directory_from_bytes(bytes: &[u8], max_entries: usize) -> Vec<ProfileEntry> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .take(max_entries)
        .map_while(|&[sector_hi, sector_lo, enabled, _]| {
            let sector = u16::from_be_bytes([sector_hi, sector_lo]);
            (sector != 0xffff).then_some((sector, enabled))
        })
        .filter_map(|(sector, enabled)| {
            let index = u8::try_from(sector & 0x00ff).ok().filter(|&i| i != 0)?;
            Some(ProfileEntry {
                index,
                sector,
                enabled: enabled != 0,
            })
        })
        .collect()
}

fn name_from_bytes(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&unit| u16::from_le_bytes(unit))
        .take_while(|&unit| unit != 0 && unit != 0xffff)
        .collect();
    let name = String::from_utf16_lossy(&units);
    let name = name.trim();
    (!name.is_empty() && name != UNNAMED_PLACEHOLDER).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_g502_lightspeed_info() {
        let mut payload = [0; 16];
        payload[..11].copy_from_slice(&[1, 3, 1, 5, 1, 0x0b, 0x10, 0x00, 0xff, 0x0a, 0x04]);
        let info = info_from_payload(&payload);
        assert_eq!(info.profile_format, 3);
        assert_eq!(info.profile_count, 5);
        assert_eq!(info.button_count, 11);
        assert_eq!(info.sector_size, 255);
    }

    #[test]
    fn decodes_directory_until_terminator() {
        let bytes = [
            0x00, 0x01, 0x01, 0xff, 0x00, 0x02, 0x00, 0xff, 0x00, 0x05, 0x01, 0xff, 0xff, 0xff,
            0xff, 0xff, 0x00, 0x03, 0x01, 0xff,
        ];
        let entries = directory_from_bytes(&bytes, 5);
        assert_eq!(
            entries,
            [
                ProfileEntry {
                    index: 1,
                    sector: 1,
                    enabled: true
                },
                ProfileEntry {
                    index: 2,
                    sector: 2,
                    enabled: false
                },
                ProfileEntry {
                    index: 5,
                    sector: 5,
                    enabled: true
                },
            ]
        );
    }

    #[test]
    fn blank_directory_is_empty() {
        assert!(directory_from_bytes(&[0xff; 16], 5).is_empty());
    }

    #[test]
    fn rom_directory_entries_keep_their_low_byte_index() {
        let entries = directory_from_bytes(&[0x01, 0x01, 0x01, 0xff, 0xff, 0xff, 0, 0], 2);
        assert_eq!(entries[0].index, 1);
        assert_eq!(entries[0].sector, 0x0101);
    }

    #[test]
    fn decodes_utf16_names_and_drops_the_placeholder() {
        let encode = |s: &str| {
            let mut bytes: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
            bytes.resize(NAME_LEN, 0xff);
            bytes
        };
        assert_eq!(name_from_bytes(&encode("Stock")), Some("Stock".into()));
        assert_eq!(name_from_bytes(&encode(UNNAMED_PLACEHOLDER)), None);
        assert_eq!(name_from_bytes(&[0xff; NAME_LEN]), None);
    }
}
