//! Implements `ForceSensingButton` (`0x19c0`) without changing device defaults.
//!
//! This layout is reverse-engineered, not a public Logitech specification.
//! Protocol reference: Solaar's `ForceSensingButton` and `ForceSensingButtonArray`:
//! <https://github.com/pwr-Solaar/Solaar/blob/e7304c4c451cc9bb4f206a914844525e67856a28/lib/logitech_receiver/hidpp20.py>.
//! Thresholds are raw device units, not percentages or gram-force values.

use std::ops::RangeInclusive;

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::FeatureEndpoint,
    protocol::v20::{self, Hidpp20Error},
};

/// A zero-based force-button index, distinct from a reprogrammable control ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, derive_more::From, derive_more::Into)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForceButtonIndex(u8);

/// An activation threshold in device-specific raw units.
///
/// All `u16` values can be observed. A write must also pass the button's range check.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::From, derive_more::Into,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForceThreshold(u16);

bitflags::bitflags! {
    /// The button's reported capabilities, including unnamed bits.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ForceButtonCapabilities: u16 {
        /// The device permits changing this button's activation threshold.
        const CHANGEABLE = 1 << 0;
    }
}

/// A force-button write refusal or protocol failure.
#[derive(Debug, thiserror::Error)]
pub enum ForceButtonError {
    /// The reported capabilities do not permit threshold changes.
    #[error("the force button is read-only")]
    ReadOnly,
    /// The requested threshold is outside the button's reported inclusive range.
    #[error("the force threshold is outside the button's range")]
    OutOfRange,
    /// The device or channel rejected the operation.
    #[error(transparent)]
    Protocol(#[from] Hidpp20Error),
}

/// Implements `ForceSensingButton` / `0x19c0` version 0.
#[derive(Clone, Feature)]
#[creatable(id = 0x19c0, version = 0)]
pub struct ForceSensingButtonFeature {
    endpoint: FeatureEndpoint,
}

impl ForceSensingButtonFeature {
    /// Reads the number of force buttons. Their indices start at zero.
    pub async fn get_button_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Reads metadata and binds a button handle to this endpoint and index.
    ///
    /// The eight-byte metadata requires a long reply. Invalid ranges or defaults
    /// are unsupported responses, not a reason to substitute model-specific values.
    pub async fn get_button_info(
        &self,
        index: ForceButtonIndex,
    ) -> Result<ForceButton, Hidpp20Error> {
        let v20::Message::Long(_, payload) = self.endpoint.call(1, [index.0, 0, 0]).await? else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        // Solaar's reverse-engineered layout is flags, default, maximum, minimum.
        let capabilities =
            ForceButtonCapabilities::from_bits_retain(u16::from_be_bytes([payload[0], payload[1]]));
        let default = ForceThreshold::from(u16::from_be_bytes([payload[2], payload[3]]));
        let max = ForceThreshold::from(u16::from_be_bytes([payload[4], payload[5]]));
        let min = ForceThreshold::from(u16::from_be_bytes([payload[6], payload[7]]));
        let range = min..=max;
        if !range.contains(&default) {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(ForceButton {
            endpoint: self.endpoint.clone(),
            index,
            capabilities,
            default,
            range,
        })
    }
}

/// A button whose writes use its own endpoint, capabilities and reported range.
///
/// Reading metadata does not write a default or arm the button. Re-probe after a
/// device reconnect; a handle does not extend the channel's device lifecycle.
#[derive(Clone)]
pub struct ForceButton {
    endpoint: FeatureEndpoint,
    index: ForceButtonIndex,
    capabilities: ForceButtonCapabilities,
    default: ForceThreshold,
    range: RangeInclusive<ForceThreshold>,
}

impl ForceButton {
    /// The zero-based index this handle addresses.
    #[must_use]
    pub const fn index(&self) -> ForceButtonIndex {
        self.index
    }

    /// The reported capabilities, retaining unnamed bits.
    #[must_use]
    pub const fn capabilities(&self) -> ForceButtonCapabilities {
        self.capabilities
    }

    /// The device-reported default, not necessarily the current threshold.
    #[must_use]
    pub const fn default_threshold(&self) -> ForceThreshold {
        self.default
    }

    /// The inclusive threshold range reported by this button.
    #[must_use]
    pub fn range(&self) -> RangeInclusive<ForceThreshold> {
        self.range.clone()
    }

    /// Reads the current activation threshold without substituting the default.
    pub async fn get_button_config(&self) -> Result<ForceThreshold, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [self.index.0, 0, 0])
            .await?
            .extend_payload();
        Ok(ForceThreshold::from(u16::from_be_bytes([
            payload[0], payload[1],
        ])))
    }

    /// Writes only a changeable button's in-range activation threshold.
    ///
    /// The reply acknowledges the request; it is not a verified current value.
    /// Use [`Self::get_button_config`] to read back the device's resulting threshold.
    pub async fn set_button_config(
        &self,
        threshold: ForceThreshold,
    ) -> Result<(), ForceButtonError> {
        if !self
            .capabilities
            .contains(ForceButtonCapabilities::CHANGEABLE)
        {
            return Err(ForceButtonError::ReadOnly);
        }
        if !self.range.contains(&threshold) {
            return Err(ForceButtonError::OutOfRange);
        }
        let [high, low] = threshold.0.to_be_bytes();
        self.endpoint.call(3, [self.index.0, high, low]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
