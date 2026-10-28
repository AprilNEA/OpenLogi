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
    /// Whether an unsolicited report with `function_id` announces that the
    /// disabled-key set changed, such as software on another host replacing
    /// it.
    ///
    /// Reverse-engineered, not from the spec: Logi Options+ 2.8 turns an
    /// unsolicited function-2 report — the [`Self::set_disabled_keys`] id —
    /// into a setting change and reads byte 0 as the new mask. No
    /// specification or capture confirms that layout, so the payload is not
    /// decoded: a listener re-reads the set with [`Self::get_disabled_keys`].
    #[must_use]
    pub const fn is_state_broadcast(function_id: u8) -> bool {
        function_id == 2
    }

    /// Retrieves the set of keys the device allows software to disable.
    pub async fn get_capabilities(&self) -> Result<DisableableKeys, Hidpp20Error> {
        self.keys(0, [0; 3], crate::protocol::WriteCompletion::Bounded)
            .await
    }

    /// Retrieves the set of keys currently disabled.
    pub async fn get_disabled_keys(&self) -> Result<DisableableKeys, Hidpp20Error> {
        self.keys(1, [0; 3], crate::protocol::WriteCompletion::Bounded)
            .await
    }

    /// Replaces the set of disabled keys and returns the device's echo.
    ///
    /// This replaces the whole set, so passing [`DisableableKeys::empty`]
    /// re-enables every key. The device rejects keys it cannot disable.
    pub async fn set_disabled_keys(
        &self,
        keys: DisableableKeys,
    ) -> Result<DisableableKeys, Hidpp20Error> {
        self.keys(
            2,
            [keys.bits(), 0, 0],
            crate::protocol::WriteCompletion::Drain,
        )
        .await
    }

    /// Capability query for an owner that drains every submitted native report.
    pub async fn get_capabilities_write_through(&self) -> Result<DisableableKeys, Hidpp20Error> {
        self.keys(0, [0; 3], crate::protocol::WriteCompletion::Drain)
            .await
    }

    /// State query for an owner that drains every submitted native report.
    pub async fn get_disabled_keys_write_through(&self) -> Result<DisableableKeys, Hidpp20Error> {
        self.keys(1, [0; 3], crate::protocol::WriteCompletion::Drain)
            .await
    }

    async fn keys(
        &self,
        function: u8,
        args: [u8; 3],
        completion: crate::protocol::WriteCompletion,
    ) -> Result<DisableableKeys, Hidpp20Error> {
        let response = match completion {
            crate::protocol::WriteCompletion::Bounded => self.endpoint.call(function, args).await,
            crate::protocol::WriteCompletion::Drain => {
                self.endpoint.call_write_through(function, args).await
            }
        }?;
        let payload = response.extend_payload();
        Ok(DisableableKeys::from_bits_retain(payload[0]))
    }
}
