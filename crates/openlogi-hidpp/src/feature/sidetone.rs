//! Implements `Sidetone` (feature `0x8300`) for audio devices.
//!
//! Version 0 already carries the level functions: the G535 (`046d:0ac4`) and
//! the G733 (`046d:0b1f`) report the feature at version 0 and answer them.

#[cfg(test)]
mod tests;

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// The highest sidetone level the feature documents.
pub const MAX_SIDETONE_LEVEL: u8 = 100;

/// Per-channel sidetone mute statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SidetoneMuteStatus {
    /// Raw mute-status bitmask. A set bit means the channel is muted.
    pub statuses: u8,
}

/// Change mask and statuses for sidetone mute settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SidetoneMuteChange {
    /// Channels to update. A set bit means the corresponding status bit applies.
    pub change_mask: u8,
    /// Desired mute statuses. A set bit means the channel should be muted.
    pub statuses: u8,
}

/// Implements the `Sidetone` / `0x8300` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x8300, version = 0)]
pub struct SidetoneFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl SidetoneFeature {
    /// Retrieves the sidetone level, in the documented `0..=100` range.
    ///
    /// A level above 100 is an [`Hidpp20Error::UnsupportedResponse`].
    pub async fn get_sidetone_level(&self) -> Result<u8, Hidpp20Error> {
        level(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Sets the sidetone level and returns the level the device echoes.
    /// Devices reject values outside `0..=100`.
    ///
    /// The echo is the confirmation to use. A G535 answers
    /// [`Self::get_sidetone_level`] with the previous level for roughly
    /// 200 ms after a write, while the dongle relays it to the headset
    /// (measured on the device, not documented).
    pub async fn set_sidetone_level(&self, level_to_set: u8) -> Result<u8, Hidpp20Error> {
        level(
            self.endpoint
                .call(1, [level_to_set, 0, 0])
                .await?
                .extend_payload()[0],
        )
    }

    /// Retrieves sidetone mute statuses.
    pub async fn get_sidetone_mute(&self) -> Result<SidetoneMuteStatus, Hidpp20Error> {
        Ok(SidetoneMuteStatus {
            statuses: self.endpoint.call(2, [0; 3]).await?.extend_payload()[0],
        })
    }

    /// Updates selected sidetone mute statuses.
    pub async fn set_sidetone_mute(&self, change: SidetoneMuteChange) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(3, [change.change_mask, change.statuses, 0])
            .await?;
        Ok(())
    }
}

fn level(raw: u8) -> Result<u8, Hidpp20Error> {
    if raw > MAX_SIDETONE_LEVEL {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(raw)
}
