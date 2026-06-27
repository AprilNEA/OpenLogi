//! Implements `ModeStatus` (feature `0x8090`).

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// The first mode-status byte.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ModeStatus0: u8 {
        /// Performance mode. When unset, the device is in endurance mode.
        const PERFORMANCE = 1 << 0;
    }
}

bitflags::bitflags! {
    /// Capabilities reported by `ModeStatus`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ModeStatusCapabilities: u16 {
        /// A hardware switch can change the mode bit.
        const HARDWARE_SWITCH = 1 << 0;
        /// Software can change the mode bit.
        const SOFTWARE_SWITCH = 1 << 1;
    }
}

/// Current mode-status bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ModeStatus {
    /// Primary status bits.
    pub status0: ModeStatus0,
    /// Secondary status byte, reserved by v1 but preserved for callers.
    pub status1: u8,
}

/// A mode-status update request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ModeStatusChange {
    /// Desired primary status bits.
    pub status0: ModeStatus0,
    /// Desired secondary status byte.
    pub status1: u8,
    /// Primary changed-bit mask.
    pub changed_mask0: ModeStatus0,
    /// Secondary changed-bit mask.
    pub changed_mask1: u8,
}

/// Implements the `ModeStatus` / `0x8090` feature.
#[derive(Clone)]
pub struct ModeStatusFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for ModeStatusFeature {
    const ID: u16 = 0x8090;
    const STARTING_VERSION: u8 = 1;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for ModeStatusFeature {}

impl ModeStatusFeature {
    /// Retrieves the current mode status.
    pub async fn get_mode_status(&self) -> Result<ModeStatus, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ModeStatus {
            status0: ModeStatus0::from_bits_retain(payload[0]),
            status1: payload[1],
        })
    }

    /// Sets selected mode-status bits.
    pub async fn set_mode_status(&self, change: ModeStatusChange) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = change.status0.bits();
        args[1] = change.status1;
        args[2] = change.changed_mask0.bits();
        args[3] = change.changed_mask1;

        self.endpoint.call_long(1, args).await?;
        Ok(())
    }

    /// Enables or disables performance mode.
    pub async fn set_performance_mode(&self, enabled: bool) -> Result<(), Hidpp20Error> {
        let status0 = if enabled {
            ModeStatus0::PERFORMANCE
        } else {
            ModeStatus0::empty()
        };
        self.set_mode_status(ModeStatusChange {
            status0,
            status1: 0,
            changed_mask0: ModeStatus0::PERFORMANCE,
            changed_mask1: 0,
        })
        .await
    }

    /// Retrieves device capabilities for mode switching.
    pub async fn get_device_config(&self) -> Result<ModeStatusCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        Ok(ModeStatusCapabilities::from_bits_retain(
            u16::from_be_bytes([payload[0], payload[1]]),
        ))
    }
}
