//! Implements the `OnboardProfiles` feature (ID `0x8100`): the profiles a
//! gaming mouse stores in its own memory, and the switch between running
//! them ([`OnboardMode::Onboard`]) and letting host software drive the
//! device live ([`OnboardMode::Host`]).
//!
//! Logitech has not published this feature. The function ids, the memory
//! layout and the sector CRC follow libratbag (`src/hidpp20.c`) and Solaar
//! (`lib/logitech_receiver/hidpp20.py`), which agree, and were checked
//! against a G403 Prodigy (`046d:c083`). [`profile`] decodes the sectors.
//!
//! All multi-byte fields in the requests are big-endian.

pub mod profile;
pub mod types;

#[cfg(test)]
mod tests;

use openlogi_hidpp_derive::Feature;

pub use types::{
    MacroFormat, MemoryModel, OnboardMode, OnboardProfilesInfo, ProfileFormat, SectorAddress,
};

use crate::{
    feature::FeatureEndpoint,
    protocol::v20::{ErrorType, Hidpp20Error},
};

/// Bytes moved by one memory read or write request.
pub const MEMORY_CHUNK: usize = 16;

/// [`MEMORY_CHUNK`] as the requests' 16-bit offsets count it.
const MEMORY_CHUNK_LEN: u16 = 16;

/// Implements the `OnboardProfiles` / `0x8100` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Retrieves the memory description (`getOnboardProfilesDescription`).
    pub async fn get_info(&self) -> Result<OnboardProfilesInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(OnboardProfilesInfo::from_payload(&payload))
    }

    /// Switches between onboard and host mode (`setOnboardMode`).
    pub async fn set_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [mode.into(), 0, 0]).await?;
        Ok(())
    }

    /// Reads the current mode (`getOnboardMode`). A value that names no
    /// mode fails with [`Hidpp20Error::UnsupportedResponse`].
    pub async fn get_mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        let raw = self.endpoint.call(2, [0; 3]).await?.extend_payload()[0];
        OnboardMode::try_from(raw).map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Activates the profile stored at `sector` (`setCurrentProfile`). The
    /// firmware reloads the profile from memory, so this also applies a
    /// profile sector just rewritten.
    pub async fn set_current_profile(&self, sector: SectorAddress) -> Result<(), Hidpp20Error> {
        let [hi, lo] = sector.raw().to_be_bytes();
        self.endpoint.call(3, [hi, lo, 0]).await?;
        Ok(())
    }

    /// Reads the sector of the active profile (`getCurrentProfile`).
    pub async fn get_current_profile(&self) -> Result<SectorAddress, Hidpp20Error> {
        let payload = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(SectorAddress::new(u16::from_be_bytes([
            payload[0], payload[1],
        ])))
    }

    /// Reads [`MEMORY_CHUNK`] bytes of `sector` from `offset`
    /// (`memoryRead`). The firmware refuses an `offset` past
    /// `sector_size - 16`.
    pub async fn read_memory(
        &self,
        sector: SectorAddress,
        offset: u16,
    ) -> Result<[u8; MEMORY_CHUNK], Hidpp20Error> {
        let mut args = [0; 16];
        args[..2].copy_from_slice(&sector.raw().to_be_bytes());
        args[2..4].copy_from_slice(&offset.to_be_bytes());
        self.endpoint.call_long(5, args).await?.extend_payload()[..MEMORY_CHUNK]
            .try_into()
            .map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Reads a whole sector of `size` bytes.
    ///
    /// Reads advance 16 bytes at a time; the firmware refuses a read that
    /// would run past the sector, so the last one starts at `size - 16` and
    /// overlaps the one before it when `size` is not a multiple of 16.
    pub async fn read_sector(
        &self,
        sector: SectorAddress,
        size: u16,
    ) -> Result<Vec<u8>, Hidpp20Error> {
        let chunk = MEMORY_CHUNK_LEN;
        if size < chunk {
            return Err(Hidpp20Error::Feature(ErrorType::InvalidArgument));
        }
        let mut data = vec![0; usize::from(size)];
        let mut offset = 0;
        while offset < size {
            let start = offset.min(size - chunk);
            let bytes = self.read_memory(sector, start).await?;
            let start = usize::from(start);
            data[start..start + MEMORY_CHUNK].copy_from_slice(&bytes);
            offset += chunk;
        }
        Ok(data)
    }

    /// Writes `data` over `sector` from offset 0, as one
    /// `memoryAddrWrite` / `memoryWrite`… / `memoryWriteEnd` transaction.
    ///
    /// The caller supplies the whole sector, its trailing CRC included; the
    /// firmware does not compute it. A length that is not a whole number of
    /// 16-byte chunks is padded with `0xff`, which is past the length the
    /// transaction declares, so the firmware discards it.
    pub async fn write_sector(
        &self,
        sector: SectorAddress,
        data: &[u8],
    ) -> Result<(), Hidpp20Error> {
        let len = u16::try_from(data.len())
            .map_err(|_| Hidpp20Error::Feature(ErrorType::InvalidArgument))?;
        let mut start = [0; 16];
        start[..2].copy_from_slice(&sector.raw().to_be_bytes());
        start[4..6].copy_from_slice(&len.to_be_bytes());
        self.endpoint.call_long(6, start).await?;
        for chunk in data.chunks(MEMORY_CHUNK) {
            let mut args = [0xff; 16];
            args[..chunk.len()].copy_from_slice(chunk);
            self.endpoint.call_long(7, args).await?;
        }
        self.endpoint.call(8, [0; 3]).await?;
        Ok(())
    }

    /// Reads the index of the active DPI slot in the active profile
    /// (`getCurrentDpiIndex`).
    pub async fn get_current_dpi_index(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0xb, [0; 3]).await?.extend_payload()[0])
    }

    /// Selects DPI slot `index` of the active profile
    /// (`setCurrentDpiIndex`).
    pub async fn set_current_dpi_index(&self, index: u8) -> Result<(), Hidpp20Error> {
        self.endpoint.call(0xc, [index, 0, 0]).await?;
        Ok(())
    }
}
