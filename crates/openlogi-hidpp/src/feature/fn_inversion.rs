//! Implements function-key inversion features.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, hosts_info::HostIndex},
    protocol::v20::{self, Hidpp20Error},
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
#[derive(Clone, Feature)]
#[creatable(id = 0x40a3, version = 0)]
pub struct FnInversionMultiHostFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl FnInversionMultiHostFeature {
    /// Retrieves global Fn inversion for `host`.
    ///
    /// The four-field reply does not fit a short report, so a short reply is
    /// refused rather than zero-filled into a capability byte of 0.
    pub async fn get_global_fn_inversion(
        &self,
        host: HostIndex,
    ) -> Result<FnInversionInfo, Hidpp20Error> {
        let v20::Message::Long(_, payload) = self.endpoint.call(0, [u8::from(host), 0, 0]).await?
        else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        FnInversionInfo::from_payload(payload)
    }

    /// Sets global Fn inversion for `host`.
    ///
    /// The setting is stored by the device for the selected host slot. Like
    /// [`Self::get_global_fn_inversion`], a short reply is refused.
    pub async fn set_global_fn_inversion(
        &self,
        host: HostIndex,
        state: FnInversionState,
    ) -> Result<FnInversionInfo, Hidpp20Error> {
        let v20::Message::Long(_, payload) = self
            .endpoint
            .call(1, set_multi_host_fn_inversion_args(host, state))
            .await?
        else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        FnInversionInfo::from_payload(payload)
    }
}

fn set_multi_host_fn_inversion_args(host: HostIndex, state: FnInversionState) -> [u8; 3] {
    [u8::from(host), u8::from(state), 0]
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

/// Global function-key inversion state, common to all keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct GlobalFnInversion {
    /// Current inversion state.
    pub state: FnInversionState,
    /// Default inversion state.
    pub default_state: FnInversionState,
}

impl GlobalFnInversion {
    fn from_payload(payload: [u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            state: FnInversionState::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            default_state: FnInversionState::try_from(payload[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
        })
    }
}

/// Implements `FnInversionWithDefaultState` / `0x40a2`.
///
/// This is the single-host predecessor of
/// [`FnInversionMultiHostFeature`] (`0x40a3`): the inversion state is global
/// rather than per host slot.
#[derive(Clone, Feature)]
#[creatable(id = 0x40a2, version = 0)]
pub struct FnInversionWithDefaultStateFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl FnInversionWithDefaultStateFeature {
    /// Retrieves the global Fn inversion state and its default.
    pub async fn get_global_fn_inversion(&self) -> Result<GlobalFnInversion, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        GlobalFnInversion::from_payload(payload)
    }

    /// Sets the global Fn inversion state and returns the resulting state.
    pub async fn set_global_fn_inversion(
        &self,
        state: FnInversionState,
    ) -> Result<GlobalFnInversion, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(1, [u8::from(state), 0, 0])
            .await?
            .extend_payload();
        GlobalFnInversion::from_payload(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FnInversionInfo, FnInversionState, GlobalFnInversion, set_multi_host_fn_inversion_args,
    };
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

    #[test]
    fn parses_global_fn_inversion() {
        let mut payload = [0; 16];
        payload[0] = 1;
        payload[1] = 0;

        let global = GlobalFnInversion::from_payload(payload).unwrap();

        assert_eq!(global.state, FnInversionState::On);
        assert_eq!(global.default_state, FnInversionState::Off);
    }

    #[test]
    fn encodes_multi_host_set_args_as_host_then_state() {
        assert_eq!(
            set_multi_host_fn_inversion_args(HostIndex::Slot(2), FnInversionState::On),
            [2, 1, 0]
        );
    }

    /// What a call gets when the device answers function `function` with
    /// `reply`.
    fn call_with<T, F>(
        function: u8,
        call: impl FnOnce(super::FnInversionMultiHostFeature) -> F,
        reply: impl FnOnce(crate::protocol::v20::MessageHeader) -> crate::protocol::v20::Message,
    ) -> Result<T, crate::protocol::v20::Hidpp20Error>
    where
        F: Future<Output = Result<T, crate::protocol::v20::Hidpp20Error>>,
    {
        use std::sync::Arc;

        use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
        use crate::feature::CreatableFeature;
        use crate::nibble::U4;
        use crate::protocol::v20::MessageHeader;

        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let mut pending =
                Box::pin(call(super::FnInversionMultiHostFeature::new(channel, 3, 9)));
            assert!(futures::poll!(pending.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 9,
                function_id: U4::from_lo(function),
                software_id: U4::from_lo(1),
            };
            handle.send_incoming(reply(header).into()).await;
            pending.await
        })
    }

    #[test]
    fn multi_host_reads_and_writes_refuse_a_short_reply() {
        use std::assert_matches;

        use super::{FnInversionCapabilities, FnInversionMultiHostFeature};
        use crate::protocol::v20::{Hidpp20Error, Message};

        // Zero-filled, this short reply would decode as host 1, state On,
        // default Off and no MANUAL_FN_LOCK capability.
        let short = |header| Message::Short(header, [1, 1, 0]);
        let mut long = [0; 16];
        long[..4].copy_from_slice(&[1, 1, 0, 1]);

        let get = |f: FnInversionMultiHostFeature| async move {
            f.get_global_fn_inversion(HostIndex::Slot(1)).await
        };
        let set = |f: FnInversionMultiHostFeature| async move {
            f.set_global_fn_inversion(HostIndex::Slot(1), FnInversionState::On)
                .await
        };

        assert_matches!(
            call_with(0, get, short),
            Err(Hidpp20Error::UnsupportedResponse)
        );
        assert_matches!(
            call_with(1, set, short),
            Err(Hidpp20Error::UnsupportedResponse)
        );

        for (function, info) in [
            (0, call_with(0, get, |header| Message::Long(header, long))),
            (1, call_with(1, set, |header| Message::Long(header, long))),
        ] {
            let info = info.unwrap_or_else(|err| panic!("function {function}: {err:?}"));
            assert_eq!(info.host_index, HostIndex::Slot(1));
            assert_eq!(info.state, FnInversionState::On);
            assert!(
                info.capabilities
                    .contains(FnInversionCapabilities::MANUAL_FN_LOCK)
            );
        }
    }

    #[test]
    fn single_host_reads_accept_a_short_reply() {
        use super::FnInversionWithDefaultStateFeature;
        use crate::protocol::v20::Message;

        // 0x40A2's two fields fit a short report, which stays valid.
        let global = futures::executor::block_on(async {
            use std::sync::Arc;

            use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
            use crate::feature::CreatableFeature;
            use crate::nibble::U4;
            use crate::protocol::v20::MessageHeader;

            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = FnInversionWithDefaultStateFeature::new(channel, 3, 9);
            let mut pending = Box::pin(feature.get_global_fn_inversion());
            assert!(futures::poll!(pending.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 9,
                function_id: U4::from_lo(0),
                software_id: U4::from_lo(1),
            };
            handle
                .send_incoming(Message::Short(header, [1, 0, 0]).into())
                .await;
            pending.await
        })
        .unwrap();
        assert_eq!(global.state, FnInversionState::On);
        assert_eq!(global.default_state, FnInversionState::Off);
    }
}
