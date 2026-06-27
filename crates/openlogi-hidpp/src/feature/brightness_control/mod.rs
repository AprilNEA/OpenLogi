//! Implements `BrightnessControl` (feature `0x8040`).

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Capabilities reported by `BrightnessControl`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct BrightnessCapabilities: u8 {
        /// Hardware can change brightness directly.
        const HARDWARE_BRIGHTNESS = 1 << 0;
        /// The device emits brightness or illumination change events.
        const EVENTS = 1 << 1;
        /// Illumination can be queried and controlled separately from brightness.
        const ILLUMINATION = 1 << 2;
        /// Hardware can toggle illumination on and off directly.
        const HARDWARE_ON_OFF = 1 << 3;
        /// Brightness is transient and not persisted by the device.
        const TRANSIENT = 1 << 4;
    }
}

/// Brightness range and capability information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BrightnessInfo {
    /// Minimum accepted brightness.
    pub min_brightness: u16,
    /// Maximum accepted brightness.
    pub max_brightness: u16,
    /// Number of brightness steps advertised by the device.
    pub steps: u16,
    /// Feature capabilities.
    pub capabilities: BrightnessCapabilities,
}

/// Implements the `BrightnessControl` / `0x8040` feature.
#[derive(Clone)]
pub struct BrightnessControlFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for BrightnessControlFeature {
    const ID: u16 = 0x8040;
    const STARTING_VERSION: u8 = 1;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for BrightnessControlFeature {}

impl BrightnessControlFeature {
    /// Retrieves brightness range and capability information.
    pub async fn get_info(&self) -> Result<BrightnessInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(BrightnessInfo::from_payload(payload))
    }

    /// Retrieves the current brightness value.
    pub async fn get_brightness(&self) -> Result<u16, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        Ok(u16::from_be_bytes([payload[0], payload[1]]))
    }

    /// Sets the current brightness value.
    pub async fn set_brightness(&self, brightness: u16) -> Result<(), Hidpp20Error> {
        let [hi, lo] = brightness.to_be_bytes();
        self.endpoint.call(2, [hi, lo, 0]).await?;
        Ok(())
    }

    /// Retrieves whether illumination is currently enabled.
    pub async fn get_illumination(&self) -> Result<bool, Hidpp20Error> {
        Ok(self.endpoint.call(3, [0; 3]).await?.extend_payload()[0] & 1 != 0)
    }

    /// Enables or disables illumination.
    pub async fn set_illumination(&self, enabled: bool) -> Result<(), Hidpp20Error> {
        self.endpoint.call(4, [u8::from(enabled), 0, 0]).await?;
        Ok(())
    }
}

impl BrightnessInfo {
    fn from_payload(payload: [u8; 16]) -> Self {
        Self {
            min_brightness: u16::from_be_bytes([payload[4], payload[5]]),
            max_brightness: u16::from_be_bytes([payload[0], payload[1]]),
            steps: u16::from_be_bytes([payload[6], payload[2]]),
            capabilities: BrightnessCapabilities::from_bits_retain(payload[3]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BrightnessCapabilities, BrightnessInfo};

    #[test]
    fn parses_split_steps_field() {
        let mut payload = [0; 16];
        payload[0..=1].copy_from_slice(&1000u16.to_be_bytes());
        payload[2] = 0x34;
        payload[3] = BrightnessCapabilities::ILLUMINATION.bits();
        payload[4..=5].copy_from_slice(&10u16.to_be_bytes());
        payload[6] = 0x12;

        let info = BrightnessInfo::from_payload(payload);

        assert_eq!(info.min_brightness, 10);
        assert_eq!(info.max_brightness, 1000);
        assert_eq!(info.steps, 0x1234);
        assert!(
            info.capabilities
                .contains(BrightnessCapabilities::ILLUMINATION)
        );
    }
}
