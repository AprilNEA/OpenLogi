//! Implements `Sidetone` (feature `0x8300`) for audio devices.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

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
#[non_exhaustive]
pub struct SidetoneMuteChange {
    /// Channels to update. A set bit means the corresponding status bit applies.
    pub change_mask: u8,
    /// Desired mute statuses. A set bit means the channel should be muted.
    pub statuses: u8,
}

/// Implements the `Sidetone` / `0x8300` feature.
#[derive(Clone)]
pub struct SidetoneFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for SidetoneFeature {
    const ID: u16 = 0x8300;
    const STARTING_VERSION: u8 = 1;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for SidetoneFeature {}

impl SidetoneFeature {
    /// Retrieves the sidetone level, in the documented `0..=100` range.
    pub async fn get_sidetone_level(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Sets the sidetone level. Devices reject values outside `0..=100`.
    pub async fn set_sidetone_level(&self, level: u8) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [level, 0, 0]).await?;
        Ok(())
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
