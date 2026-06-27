//! Implements function-key inversion features.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint, hosts_info::HostIndex},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Function-key inversion capabilities.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct FnInversionCapabilities: u8 {
        /// The device supports manual Fn-lock control.
        const MANUAL_FN_LOCK = 1 << 0;
    }
}

/// Function-key inversion state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum FnInversionState {
    /// Function-key inversion is disabled.
    Off = 0,
    /// Function-key inversion is enabled.
    On = 1,
}

impl From<bool> for FnInversionState {
    fn from(value: bool) -> Self {
        if value { Self::On } else { Self::Off }
    }
}

/// Function-key inversion state for a host slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FnInversionInfo {
    /// Host slot index returned by the device.
    pub host_index: HostIndex,
    /// Current inversion state.
    pub state: FnInversionState,
    /// Default inversion state.
    pub default_state: FnInversionState,
    /// Inversion capabilities.
    pub capabilities: FnInversionCapabilities,
}

/// Implements `FnInversionForMultiHostDevices` / `0x40a3`.
#[derive(Clone)]
pub struct FnInversionMultiHostFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for FnInversionMultiHostFeature {
    const ID: u16 = 0x40a3;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for FnInversionMultiHostFeature {}

impl FnInversionMultiHostFeature {
    /// Retrieves global Fn inversion for `host`.
    pub async fn get_global_fn_inversion(
        &self,
        host: HostIndex,
    ) -> Result<FnInversionInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(0, [u8::from(host), 0, 0])
            .await?
            .extend_payload();
        FnInversionInfo::from_payload(payload)
    }

    /// Sets global Fn inversion for `host`.
    ///
    /// The setting is stored by the device for the selected host slot.
    pub async fn set_global_fn_inversion(
        &self,
        host: HostIndex,
        state: FnInversionState,
    ) -> Result<FnInversionInfo, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [u8::from(state), u8::from(host), 0])
            .await?
            .extend_payload();
        FnInversionInfo::from_payload(payload)
    }
}

impl FnInversionInfo {
    fn from_payload(payload: [u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            host_index: HostIndex::from(payload[0]),
            state: FnInversionState::try_from(payload[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            default_state: FnInversionState::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            capabilities: FnInversionCapabilities::from_bits_retain(payload[3]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FnInversionInfo, FnInversionState};
    use crate::feature::hosts_info::HostIndex;

    #[test]
    fn parses_fn_inversion_info() {
        let mut payload = [0; 16];
        payload[0] = 1;
        payload[1] = 1;
        payload[2] = 0;
        payload[3] = 1;

        let info = FnInversionInfo::from_payload(payload).unwrap();

        assert_eq!(info.host_index, HostIndex::Slot(1));
        assert_eq!(info.state, FnInversionState::On);
        assert_eq!(info.default_state, FnInversionState::Off);
    }
}
