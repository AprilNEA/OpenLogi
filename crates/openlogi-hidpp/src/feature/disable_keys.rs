//! Implements the `DisableKeys` feature (ID `0x4521`) that disables a fixed set
//! of lock / system keys.
//!
//! For disabling arbitrary keys by HID usage, see
//! [`DisableKeysByUsage`](super::disable_keys_by_usage) (`0x4522`).

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

bitflags::bitflags! {
    /// The set of keys a [`DisableKeysFeature`] device can disable.
    ///
    /// Used both for the device's capabilities and for the currently disabled
    /// keys.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct DisableableKeys: u8 {
        /// The Caps Lock key.
        const CAPS_LOCK = 1 << 0;
        /// The Num Lock key.
        const NUM_LOCK = 1 << 1;
        /// The Scroll Lock key.
        const SCROLL_LOCK = 1 << 2;
        /// The Insert key.
        const INSERT = 1 << 3;
        /// The Windows / Start key.
        const WINDOWS = 1 << 4;
    }
}

/// Implements the `DisableKeys` / `0x4521` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x4521, version = 0)]
pub struct DisableKeysFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl DisableKeysFeature {
    /// Retrieves the set of keys the device allows software to disable.
    pub async fn get_capabilities(&self) -> Result<DisableableKeys, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(DisableableKeys::from_bits_retain(payload[0]))
    }

    /// Retrieves the set of keys currently disabled.
    pub async fn get_disabled_keys(&self) -> Result<DisableableKeys, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        Ok(DisableableKeys::from_bits_retain(payload[0]))
    }

    /// Replaces the set of disabled keys and returns the device's echo.
    ///
    /// This replaces the whole set, so passing [`DisableableKeys::empty`]
    /// re-enables every key. The device rejects keys it cannot disable.
    pub async fn set_disabled_keys(
        &self,
        keys: DisableableKeys,
    ) -> Result<DisableableKeys, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [keys.bits(), 0, 0])
            .await?
            .extend_payload();
        Ok(DisableableKeys::from_bits_retain(payload[0]))
    }
}
