//! Implements the `OnboardProfiles` feature (ID `0x8100`).
//!
//! Reverse-engineered (libratbag), checked on a G502 LIGHTSPEED and a PRO X
//! SUPERLIGHT. Flash is written only through [`OnboardProfilesFeature::write_profile`],
//! one whole CRC-sealed profile sector at a time.

use std::num::{NonZeroU8, NonZeroU16};

use crc::{CRC_16_IBM_3740, Crc};

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

const FN_GET_INFO: u8 = 0;
const FN_SET_MODE: u8 = 1;
const FN_GET_MODE: u8 = 2;
const FN_SET_CURRENT_PROFILE: u8 = 3;
const FN_GET_CURRENT_PROFILE: u8 = 4;
const FN_MEMORY_READ: u8 = 5;
const FN_MEMORY_ADDR_WRITE: u8 = 6;
const FN_MEMORY_WRITE: u8 = 7;
const FN_MEMORY_WRITE_END: u8 = 8;

const USER_DIRECTORY_SECTOR: u16 = 0x0000;
const ROM_DIRECTORY_SECTOR: u16 = 0x0100;
const READ_CHUNK: u16 = 16;
// UTF-16LE name in profile formats 2–5.
const NAME_OFFSET: u16 = 0xa0;
const NAME_LEN: usize = 48;
// What G HUB writes into an unnamed profile.
const UNNAMED_PLACEHOLDER: &str = "PROFILE_NAME_DEFAULT";
// The head profile formats 1–5 share (libratbag): report rate, the default
// and shift DPI slot indices, then five little-endian DPI slots.
const DEFAULT_DPI_SLOT_OFFSET: usize = 1;
const DPI_TABLE_OFFSET: usize = 3;
/// DPI slots a stored profile holds.
pub const DPI_SLOTS: usize = 5;
// A sector ends in a big-endian CRC-16/CCITT-FALSE over the bytes before it.
const SECTOR_CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);
const CRC_LEN: usize = 2;

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
    pub index: NonZeroU8,
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
    pub async fn get_current_profile(&self) -> Result<Option<NonZeroU8>, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_CURRENT_PROFILE, [0; 3])
            .await?
            .extend_payload();
        Ok(NonZeroU8::new(payload[1]))
    }

    /// Makes the enabled profile `index` (1-based) current.
    pub async fn set_current_profile(&self, index: NonZeroU8) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(FN_SET_CURRENT_PROFILE, [0, index.get(), 0])
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

    /// Reads the whole profile stored in `sector` and checks its CRC.
    ///
    /// Fails with [`Hidpp20Error::UnsupportedResponse`] for a profile format
    /// whose layout is unknown, or a sector whose CRC does not match.
    pub async fn read_profile(
        &self,
        info: &OnboardProfilesInfo,
        sector: u16,
    ) -> Result<ProfileSector, Hidpp20Error> {
        if !(1..=5).contains(&info.profile_format) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        let size = usize::from(info.sector_size);
        let mut bytes = vec![0; size];
        for start in (0..size).step_by(usize::from(READ_CHUNK)) {
            // A sector size that is no multiple of 16 ends with a read that
            // overlaps the previous one, so no read runs past the sector.
            let start = start.min(size.saturating_sub(usize::from(READ_CHUNK)));
            let offset = u16::try_from(start).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
            let chunk = self.read_memory(sector, offset).await?;
            let end = (start + chunk.len()).min(size);
            bytes[start..end].copy_from_slice(&chunk[..end - start]);
        }
        ProfileSector::from_bytes(sector, bytes).ok_or(Hidpp20Error::UnsupportedResponse)
    }

    /// Writes `profile` back to its sector, sealed with a fresh CRC.
    ///
    /// The sector is written whole: start, every 16-byte chunk, end. The owner
    /// must drive this future to completion, since an abandoned write leaves
    /// a sector the firmware rejects until it is written again.
    pub async fn write_profile(&self, profile: &ProfileSector) -> Result<(), Hidpp20Error> {
        let bytes = &profile.bytes;
        let size = u16::try_from(bytes.len()).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
        let [sector_hi, sector_lo] = profile.sector.to_be_bytes();
        let [size_hi, size_lo] = size.to_be_bytes();
        let mut args = [0; 16];
        args[..6].copy_from_slice(&[sector_hi, sector_lo, 0, 0, size_hi, size_lo]);
        self.endpoint
            .call_long_write_through(FN_MEMORY_ADDR_WRITE, args)
            .await?;
        for chunk in bytes.chunks(usize::from(READ_CHUNK)) {
            let mut args = [0xff; 16];
            args[..chunk.len()].copy_from_slice(chunk);
            self.endpoint
                .call_long_write_through(FN_MEMORY_WRITE, args)
                .await?;
        }
        self.endpoint
            .call_write_through(FN_MEMORY_WRITE_END, [0; 3])
            .await?;
        Ok(())
    }
}

/// One stored profile sector. Its CRC checked out when it was read, and every
/// edit reseals it, so the bytes held are always the bytes flash should hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileSector {
    sector: u16,
    bytes: Vec<u8>,
}

impl ProfileSector {
    fn from_bytes(sector: u16, bytes: Vec<u8>) -> Option<Self> {
        let body_len = bytes.len().checked_sub(CRC_LEN)?;
        if body_len < DPI_TABLE_OFFSET + DPI_SLOTS * 2 {
            return None;
        }
        let stored = u16::from_be_bytes([bytes[body_len], bytes[body_len + 1]]);
        if SECTOR_CRC.checksum(&bytes[..body_len]) != stored {
            return None;
        }
        if usize::from(bytes[DEFAULT_DPI_SLOT_OFFSET]) >= DPI_SLOTS {
            return None;
        }
        Some(Self { sector, bytes })
    }

    /// The flash sector the profile lives in.
    #[must_use]
    pub fn sector(&self) -> u16 {
        self.sector
    }

    /// The five DPI slots; `None` for an unused one.
    #[must_use]
    pub fn dpi_slots(&self) -> [Option<NonZeroU16>; DPI_SLOTS] {
        std::array::from_fn(|slot| {
            let at = DPI_TABLE_OFFSET + slot * 2;
            let dpi = u16::from_le_bytes([self.bytes[at], self.bytes[at + 1]]);
            NonZeroU16::new(dpi).filter(|dpi| dpi.get() != 0xffff)
        })
    }

    /// The slot the profile starts in.
    #[must_use]
    pub fn default_dpi_slot(&self) -> usize {
        usize::from(self.bytes[DEFAULT_DPI_SLOT_OFFSET])
    }

    /// The DPI the profile starts at, or `None` if its default slot is unused.
    #[must_use]
    pub fn default_dpi(&self) -> Option<NonZeroU16> {
        self.dpi_slots()[self.default_dpi_slot()]
    }

    /// Makes `dpi` the profile's startup DPI. A slot that already holds it
    /// becomes the default; otherwise `dpi` replaces the value in the default
    /// slot, so the rest of the table is untouched either way.
    pub fn set_default_dpi(&mut self, dpi: NonZeroU16) {
        let holding = (0u8..)
            .zip(self.dpi_slots())
            .find_map(|(slot, held)| (held == Some(dpi)).then_some(slot));
        if let Some(slot) = holding {
            self.bytes[DEFAULT_DPI_SLOT_OFFSET] = slot;
        } else {
            let at = DPI_TABLE_OFFSET + self.default_dpi_slot() * 2;
            self.bytes[at..at + 2].copy_from_slice(&dpi.get().to_le_bytes());
        }
        self.seal();
    }

    fn seal(&mut self) {
        let body_len = self.bytes.len() - CRC_LEN;
        let crc = SECTOR_CRC.checksum(&self.bytes[..body_len]);
        self.bytes[body_len..].copy_from_slice(&crc.to_be_bytes());
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
            let index = NonZeroU8::new(u8::try_from(sector & 0x00ff).ok()?)?;
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
                    index: nz(1),
                    sector: 1,
                    enabled: true
                },
                ProfileEntry {
                    index: nz(2),
                    sector: 2,
                    enabled: false
                },
                ProfileEntry {
                    index: nz(5),
                    sector: 5,
                    enabled: true
                },
            ]
        );
    }

    /// A 255-byte sector (the G502 LIGHTSPEED size) holding `slots` and
    /// starting in `default`, sealed with a valid CRC.
    fn sector_with(slots: [u16; DPI_SLOTS], default: u8) -> ProfileSector {
        let mut bytes = vec![0xff; 255];
        bytes[0] = 4;
        bytes[DEFAULT_DPI_SLOT_OFFSET] = default;
        bytes[2] = 0;
        for (slot, dpi) in slots.iter().enumerate() {
            let at = DPI_TABLE_OFFSET + slot * 2;
            bytes[at..at + 2].copy_from_slice(&dpi.to_le_bytes());
        }
        let mut sector = ProfileSector { sector: 1, bytes };
        sector.seal();
        ProfileSector::from_bytes(1, sector.bytes).unwrap()
    }

    fn dpi(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    #[test]
    fn sector_crc_is_ccitt_false() {
        assert_eq!(SECTOR_CRC.checksum(b"123456789"), 0x29b1);
    }

    #[test]
    fn reads_the_dpi_table_and_its_default() {
        let sector = sector_with([400, 1200, 1950, 3250, 0xffff], 3);
        assert_eq!(
            sector.dpi_slots(),
            [
                Some(dpi(400)),
                Some(dpi(1200)),
                Some(dpi(1950)),
                Some(dpi(3250)),
                None
            ]
        );
        assert_eq!(sector.default_dpi(), Some(dpi(3250)));
    }

    #[test]
    fn rejects_a_sector_whose_crc_does_not_match() {
        let mut bytes = sector_with([400, 800, 1600, 3200, 6400], 2).bytes;
        bytes[DPI_TABLE_OFFSET] ^= 1;
        assert_eq!(ProfileSector::from_bytes(1, bytes), None);
    }

    #[test]
    fn rejects_a_default_slot_outside_the_table() {
        let mut sector = sector_with([400, 800, 1600, 3200, 6400], 0);
        sector.bytes[DEFAULT_DPI_SLOT_OFFSET] = 5;
        sector.seal();
        assert_eq!(ProfileSector::from_bytes(1, sector.bytes), None);
    }

    #[test]
    fn a_held_dpi_becomes_the_default_without_touching_the_table() {
        let mut sector = sector_with([400, 1200, 1950, 3250, 4000], 3);
        let table = sector.dpi_slots();
        sector.set_default_dpi(dpi(1200));
        assert_eq!(sector.default_dpi_slot(), 1);
        assert_eq!(sector.dpi_slots(), table);
    }

    #[test]
    fn a_new_dpi_replaces_only_the_default_slot() {
        let mut sector = sector_with([400, 800, 1600, 3200, 6400], 2);
        sector.set_default_dpi(dpi(1200));
        assert_eq!(sector.default_dpi_slot(), 2);
        assert_eq!(
            sector.dpi_slots(),
            [
                Some(dpi(400)),
                Some(dpi(800)),
                Some(dpi(1200)),
                Some(dpi(3200)),
                Some(dpi(6400))
            ]
        );
    }

    #[test]
    fn an_edited_sector_reseals_and_keeps_every_other_byte() {
        let original = sector_with([400, 1200, 1950, 3250, 4000], 3);
        let mut edited = original.clone();
        edited.set_default_dpi(dpi(1200));
        // What flash holds after a write must read back equal to the edit.
        let reread = ProfileSector::from_bytes(1, edited.bytes.clone()).unwrap();
        assert_eq!(reread, edited);
        let sealed = edited.bytes;
        let before = original.bytes;
        let changed: Vec<usize> = (0..before.len())
            .filter(|&i| before[i] != sealed[i])
            .collect();
        assert_eq!(changed[0], DEFAULT_DPI_SLOT_OFFSET);
        assert!(changed[1..].iter().all(|&i| i >= before.len() - CRC_LEN));
    }

    /// Profile 1 as read from a PRO X SUPERLIGHT (format 4, 255-byte sectors):
    /// slots 400/800/1600/3200/4950, starting in slot 4. The device layer's
    /// scripted mouse serves the same file.
    const SUPERLIGHT_PROFILE: &[u8] = include_bytes!("onboard_profiles/superlight_profile_1.bin");
    #[test]
    fn rewrites_a_superlight_profile_the_way_its_flash_stored_it() {
        let bytes = SUPERLIGHT_PROFILE.to_vec();
        let mut sector = ProfileSector::from_bytes(1, bytes.clone()).unwrap();
        assert_eq!(sector.default_dpi(), Some(dpi(4950)));
        sector.set_default_dpi(dpi(1200));
        let changed: Vec<usize> = (0..bytes.len())
            .filter(|&i| bytes[i] != sector.bytes[i])
            .collect();
        // Measured: the device then held slot 4 = 1200 and CRC 0x4682.
        assert_eq!(changed, [11, 12, 253, 254]);
        assert_eq!(sector.bytes[253..], [0x46, 0x82]);
        assert_eq!(sector.default_dpi(), Some(dpi(1200)));
    }

    fn nz(index: u8) -> NonZeroU8 {
        NonZeroU8::new(index).unwrap()
    }

    #[test]
    fn blank_directory_is_empty() {
        assert!(directory_from_bytes(&[0xff; 16], 5).is_empty());
    }

    #[test]
    fn rom_directory_entries_keep_their_low_byte_index() {
        let entries = directory_from_bytes(&[0x01, 0x01, 0x01, 0xff, 0xff, 0xff, 0, 0], 2);
        assert_eq!(entries[0].index, nz(1));
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
