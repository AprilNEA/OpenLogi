//! Implements the `ForceSensingButton` feature (`0x19c0`) — the
//! force-sensitive pad first seen as the MX Master 4's Action Ring button.
//!
//! The function map of this feature is NOT publicly documented. Everything
//! here is reverse-engineered against real hardware; this wrapper therefore
//! exposes a deliberately raw probe surface so callers can map the function
//! table, and typed accessors should replace [`Self::raw_call`] as layouts are
//! confirmed on hardware. Keep the reverse-engineering annotations honest.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements `ForceSensingButton` / `0x19c0`.
#[derive(Clone)]
pub struct ForceSensingButtonFeature {
    /// The endpoint this feature talks to.
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
    /// Raw short-form call: sends `function` with three argument bytes and
    /// returns the 16-byte response payload verbatim.
    ///
    /// Reverse-engineering aid — the caller owns interpretation. A device
    /// rejecting an unknown function returns the HID++ 2.0 `InvalidFunction`
    /// error rather than garbage, so probing function IDs is safe.
    pub async fn raw_call(&self, function: u8, args: [u8; 3]) -> Result<[u8; 16], Hidpp20Error> {
        Ok(self.endpoint.call(function, args).await?.extend_payload())
    }

    /// Sets `button`'s force-activation threshold (function `3`).
    ///
    /// Reverse-engineered on a real MX Master 4 (`wpid=b042`): the pad is
    /// dormant until a threshold is written, and Options+ writes `0x15a3` for
    /// button `0` at startup — the call that arms the Action Ring pad.
    /// Function `2` with the same button index reads the value back.
    pub async fn set_force_threshold(
        &self,
        button: u8,
        threshold: u16,
    ) -> Result<(), Hidpp20Error> {
        let [hi, lo] = threshold.to_be_bytes();
        self.endpoint.call(3, [button, hi, lo]).await?;
        Ok(())
    }
}
