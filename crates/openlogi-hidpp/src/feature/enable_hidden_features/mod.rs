//! Implements the `EnableHiddenFeatures` feature (`0x1e00`).
//!
//! A one-byte gate for engineering/optional functionality: some devices keep
//! auxiliary event sources dormant until a host enables this feature. Vendor
//! software (Logi Options+) is believed to enable it at session start; the
//! function layout below (get = function 0, set = function 1, enabled flag in
//! byte 0) matches the de-facto layout used by Solaar and libratbag.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements `EnableHiddenFeatures` / `0x1e00`.
#[derive(Clone)]
pub struct EnableHiddenFeaturesFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for EnableHiddenFeaturesFeature {
    const ID: u16 = 0x1e00;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for EnableHiddenFeaturesFeature {}

impl EnableHiddenFeaturesFeature {
    /// Reads whether hidden features are currently enabled (`getEnabled`,
    /// function `0`).
    pub async fn get_enabled(&self) -> Result<bool, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(payload[0] != 0)
    }

    /// Enables or disables hidden features (`setEnabled`, function `1`).
    ///
    /// The device does not persist this across power cycles.
    pub async fn set_enabled(&self, enabled: bool) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [u8::from(enabled), 0, 0]).await?;
        Ok(())
    }
}
