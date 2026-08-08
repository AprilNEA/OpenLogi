//! Implements the reverse-engineered `ForceSensingButton` feature (`0x19c0`).
//!
//! The function and payload layouts are cross-checked against Solaar and an MX
//! Master 4. Logitech has not published this feature in the public HID++ spec.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Force threshold value used by a force-sensitive button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ForceThreshold(u16);

impl ForceThreshold {
    /// Construct a threshold from its device-native value.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Return the device-native threshold value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Static limits and mutability of one force-sensitive button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ForceButtonInfo {
    /// Whether firmware allows the threshold to be changed.
    pub changeable: bool,
    /// Factory default threshold.
    pub default: ForceThreshold,
    /// Largest accepted threshold.
    pub maximum: ForceThreshold,
    /// Smallest accepted threshold.
    pub minimum: ForceThreshold,
}

impl ForceButtonInfo {
    /// Whether `threshold` is inside the device-reported range.
    #[must_use]
    pub fn accepts(self, threshold: ForceThreshold) -> bool {
        (self.minimum..=self.maximum).contains(&threshold)
    }
}

/// Implements `ForceSensingButton` / `0x19c0`.
#[derive(Clone)]
pub struct ForceSensingButtonFeature {
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for ForceSensingButtonFeature {
    const ID: u16 = 0x19c0;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for ForceSensingButtonFeature {}

impl ForceSensingButtonFeature {
    /// Return the number of force-sensitive buttons exposed by the device.
    pub async fn get_button_count(&self) -> Result<u8, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(payload[0])
    }

    /// Read static threshold information for `button`.
    pub async fn get_button_info(&self, button: u8) -> Result<ForceButtonInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [button, 0, 0])
            .await?
            .extend_payload();
        let read = |offset: usize| u16::from_be_bytes([payload[offset], payload[offset + 1]]);
        let info = ForceButtonInfo {
            changeable: read(0) & 1 != 0,
            default: ForceThreshold::new(read(2)),
            maximum: ForceThreshold::new(read(4)),
            minimum: ForceThreshold::new(read(6)),
        };
        if info.minimum > info.maximum || !info.accepts(info.default) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(info)
    }

    /// Read the current threshold for `button`.
    pub async fn get_threshold(&self, button: u8) -> Result<ForceThreshold, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [button, 0, 0])
            .await?
            .extend_payload();
        Ok(ForceThreshold::new(u16::from_be_bytes([
            payload[0], payload[1],
        ])))
    }

    /// Set the current threshold for `button`.
    ///
    /// Callers should validate the value against [`Self::get_button_info`]
    /// before writing; firmware remains the final authority.
    pub async fn set_threshold(
        &self,
        button: u8,
        threshold: ForceThreshold,
    ) -> Result<(), Hidpp20Error> {
        let [high, low] = threshold.get().to_be_bytes();
        self.endpoint.call(3, [button, high, low]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_limits_are_inclusive() {
        let info = ForceButtonInfo {
            changeable: true,
            default: ForceThreshold::new(50),
            minimum: ForceThreshold::new(10),
            maximum: ForceThreshold::new(90),
        };
        assert!(info.accepts(ForceThreshold::new(10)));
        assert!(info.accepts(ForceThreshold::new(90)));
        assert!(!info.accepts(ForceThreshold::new(91)));
    }
}
