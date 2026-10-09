//! Gaming onboard memory (`0x8100`).
//!
//! The public Logitech feature list names this feature but does not publish its
//! memory layout. Command framing and descriptor fields here are based on the
//! reverse-engineered libratbag `src/hidpp20.c` implementation (MIT), rather
//! than presented as an official specification. Profile decoding belongs to
//! the device layer, which validates the advertised format before any write.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Firmware execution mode; reading it never changes the active profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum OnboardMode {
    /// Firmware executes settings from onboard memory.
    Onboard = 1,
    /// A host application owns the device configuration.
    Host = 2,
}

/// Raw descriptor. Unknown formats remain visible for diagnosis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileDescriptor {
    /// Memory protocol generation.
    pub memory_model: u8,
    /// Profile byte-layout generation.
    pub profile_format: u8,
    /// Macro bytecode generation.
    pub macro_format: u8,
    /// Number of writable profile slots.
    pub profile_count: u8,
    /// Number of factory profile slots.
    pub factory_profile_count: u8,
    /// Number of button entries in each layer.
    pub button_count: u8,
    /// Number of writable memory sectors.
    pub sector_count: u8,
    /// Bytes per sector, including CRC.
    pub sector_size: u16,
    /// Mechanical layout identifier.
    pub mechanical_layout: u8,
    /// Uninterpreted capability bits.
    pub capabilities: u8,
}

impl ProfileDescriptor {
    fn decode(p: [u8; 16]) -> Result<Self, Hidpp20Error> {
        let sector_size = u16::from_be_bytes([p[7], p[8]]);
        if !(16..=4096).contains(&sector_size) || p[3] == 0 || p[3] > p[6] {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(Self {
            memory_model: p[0],
            profile_format: p[1],
            macro_format: p[2],
            profile_count: p[3],
            factory_profile_count: p[4],
            button_count: p[5],
            sector_count: p[6],
            sector_size,
            mechanical_layout: p[9],
            capabilities: p[10],
        })
    }
}

/// Typed endpoint for gaming onboard configuration.
#[derive(Clone, Feature)]
#[creatable(id = 0x8100, version = 0)]
pub struct OnboardProfilesFeature {
    endpoint: FeatureEndpoint,
}

impl OnboardProfilesFeature {
    /// Read the descriptor without changing host/onboard mode.
    pub async fn descriptor(&self) -> Result<ProfileDescriptor, Hidpp20Error> {
        ProfileDescriptor::decode(self.endpoint.call(0, [0; 3]).await?.extend_payload())
    }

    /// Read the execution mode.
    pub async fn mode(&self) -> Result<OnboardMode, Hidpp20Error> {
        OnboardMode::try_from(self.endpoint.call(2, [0; 3]).await?.extend_payload()[0])
            .map_err(|_| Hidpp20Error::UnsupportedResponse)
    }

    /// Select host or onboard execution, without programming flash.
    pub async fn set_mode(&self, mode: OnboardMode) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [mode.into(), 0, 0]).await?;
        Ok(())
    }

    /// Read the active one-based profile sector.
    pub async fn active_profile(&self) -> Result<u16, Hidpp20Error> {
        let p = self.endpoint.call(4, [0; 3]).await?.extend_payload();
        Ok(u16::from_be_bytes([p[0], p[1]]))
    }

    /// Select an existing profile sector, without programming flash.
    pub async fn set_active_profile(&self, sector: u16) -> Result<(), Hidpp20Error> {
        let [hi, lo] = sector.to_be_bytes();
        self.endpoint.call(3, [hi, lo, 0]).await?;
        Ok(())
    }

    /// Read one 16-byte block; the response contains data only.
    pub async fn read_block(&self, sector: u16, offset: u16) -> Result<[u8; 16], Hidpp20Error> {
        let mut p = [0; 16];
        p[..2].copy_from_slice(&sector.to_be_bytes());
        p[2..4].copy_from_slice(&offset.to_be_bytes());
        Ok(self.endpoint.call_long(5, p).await?.extend_payload())
    }

    /// Read a complete sector, including a possible overlapping tail block.
    pub async fn read_sector(&self, sector: u16, size: u16) -> Result<Vec<u8>, Hidpp20Error> {
        if !(16..=4096).contains(&size) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        let mut out = vec![0; usize::from(size)];
        for offset in block_offsets(size) {
            let data = self.read_block(sector, offset).await?;
            out[usize::from(offset)..usize::from(offset) + 16].copy_from_slice(&data);
        }
        Ok(out)
    }

    /// Program exactly one sector. Callers must validate layout, address and CRC,
    /// preserve a backup, exclude other writers, then verify by reading it back.
    /// No retries are made: a failed transfer has an uncertain hardware outcome.
    pub async fn write_sector(&self, sector: u16, data: &[u8]) -> Result<(), Hidpp20Error> {
        if !(16..=4096).contains(&data.len()) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        let size = u16::try_from(data.len()).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
        let mut start = [0; 16];
        start[..2].copy_from_slice(&sector.to_be_bytes());
        start[4..6].copy_from_slice(&size.to_be_bytes());
        self.endpoint.call_long(6, start).await?;
        for chunk in data.chunks(16) {
            // Firmware's declared count excludes padding in the final report.
            let mut payload = [0xff; 16];
            payload[..chunk.len()].copy_from_slice(chunk);
            self.endpoint.call_long(7, payload).await?;
        }
        self.endpoint.call(8, [0; 3]).await?;
        Ok(())
    }
}

fn block_offsets(size: u16) -> Vec<u16> {
    (0..size)
        .step_by(16)
        .map(|offset| offset.min(size - 16))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_uses_big_endian_size_and_rejects_unbounded_reads() {
        let mut data = [1, 5, 1, 5, 1, 13, 8, 1, 0, 0, 3, 0, 0, 0, 0, 0];
        let desc = ProfileDescriptor::decode(data).unwrap();
        assert_eq!(desc.sector_size, 256);
        assert_eq!(desc.button_count, 13);
        data[7] = 0xff;
        ProfileDescriptor::decode(data).expect_err("invalid input must be rejected");
        data[7] = 0;
        ProfileDescriptor::decode(data).expect_err("invalid input must be rejected");
    }

    #[test]
    fn short_tail_never_reads_past_sector_end() {
        assert_eq!(block_offsets(31), vec![0, 15]);
        assert_eq!(block_offsets(16), vec![0]);
        assert_eq!(block_offsets(256).last(), Some(&240));
        assert_eq!(block_offsets(254).last(), Some(&238));
    }

    #[test]
    fn unknown_mode_is_not_silently_host_mode() {
        OnboardMode::try_from(0).expect_err("invalid input must be rejected");
        OnboardMode::try_from(3).expect_err("invalid input must be rejected");
    }
}
