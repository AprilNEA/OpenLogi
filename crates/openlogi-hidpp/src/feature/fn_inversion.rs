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
    /// Whether an unsolicited `message` on this feature's index announces a
    /// Fn-inversion change, such as the user toggling Fn lock with Fn+Esc.
    ///
    /// Reverse-engineered, not from the spec: Logi Options+ 2.8
    /// (`Feature40A3FnInversion::onFeatureReport`) decodes every unsolicited
    /// report on `0x40a3` as the four [`Self::get_global_fn_inversion`] fields
    /// without looking at the function id, and refuses one shorter than four
    /// bytes. A short report cannot carry those fields, so only a long one
    /// counts. The payload is not decoded: a listener re-reads the state.
    #[must_use]
    pub const fn is_state_broadcast(message: &v20::Message) -> bool {
        matches!(message, v20::Message::Long(..))
    }

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
    /// Whether an unsolicited report with `function_id` announces a
    /// Fn-inversion change, such as the user toggling Fn lock with Fn+Esc.
    ///
    /// Reverse-engineered, not from the spec: Logi Options+ 2.8
    /// (`Feature40A2FnInversion::onUnexpectedResponse`) turns an unsolicited
    /// function-1 report — the [`Self::set_global_fn_inversion`] id — into a
    /// setting change. The payload is not decoded: a listener re-reads the
    /// state.
    #[must_use]
    pub const fn is_state_broadcast(function_id: u8) -> bool {
        function_id == 1
    }

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

/// Implements the original `FnInversion` / `0x40a0`.
///
/// The oldest of the three Fn-inversion features: one global state, with no
/// default state and no host slots. This layout is reverse-engineered from
/// Logi Options+ 2.8 (`devio::Feature40A0FnInversion`), not read from a spec:
/// function 0 answers the state in the low nibble of byte 0, and function 1
/// takes the state as its only argument byte and answers no state.
#[derive(Clone, Feature)]
#[creatable(id = 0x40a0, version = 0)]
pub struct FnInversionFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl FnInversionFeature {
    /// Retrieves the global Fn inversion state.
    ///
    /// Only the low nibble of byte 0 is the state; Options+ masks off the
    /// rest the same way. A low nibble other than 0 or 1 is refused, where
    /// Options+ would read it as off.
    pub async fn get_global_fn_inversion(&self) -> Result<FnInversionState, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        legacy_fn_inversion_state(payload[0])
    }

    /// Sets the global Fn inversion state.
    ///
    /// The reply carries no state, so a caller that needs to know what the
    /// keyboard stored reads it back with [`Self::get_global_fn_inversion`].
    pub async fn set_global_fn_inversion(
        &self,
        state: FnInversionState,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [u8::from(state), 0, 0]).await?;
        Ok(())
    }
}

/// The `0x40a0` state in the low nibble of `byte`.
fn legacy_fn_inversion_state(byte: u8) -> Result<FnInversionState, Hidpp20Error> {
    FnInversionState::try_from(byte & 0x0f).map_err(|_| Hidpp20Error::UnsupportedResponse)
}

#[cfg(test)]
mod tests {
    use super::{
        FnInversionInfo, FnInversionState, GlobalFnInversion, legacy_fn_inversion_state,
        set_multi_host_fn_inversion_args,
    };
    use crate::{feature::hosts_info::HostIndex, protocol::v20::Hidpp20Error};

    #[test]
    fn legacy_state_is_the_low_nibble_of_byte_zero() {
        use std::assert_matches;

        assert_matches!(legacy_fn_inversion_state(0x00), Ok(FnInversionState::Off));
        assert_matches!(legacy_fn_inversion_state(0x01), Ok(FnInversionState::On));
        // The high nibble is not the state; Options+ masks it off too.
        assert_matches!(legacy_fn_inversion_state(0xf0), Ok(FnInversionState::Off));
        assert_matches!(legacy_fn_inversion_state(0x31), Ok(FnInversionState::On));
        // An undefined state is an error, not "off".
        for byte in [0x02, 0x0f, 0xf2] {
            assert_matches!(
                legacy_fn_inversion_state(byte),
                Err(Hidpp20Error::UnsupportedResponse),
                "byte {byte:#04x}"
            );
        }
    }

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

    #[test]
    fn multi_host_state_broadcast_is_any_long_report() {
        use super::FnInversionMultiHostFeature;
        use crate::nibble::U4;
        use crate::protocol::v20::{Message, MessageHeader};

        let header = |function| MessageHeader {
            device_index: 1,
            feature_index: 9,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(0),
        };
        // Options+ does not look at the function id, so neither do we.
        for function in [0, 1, 2, 0x0f] {
            assert!(
                FnInversionMultiHostFeature::is_state_broadcast(&Message::Long(
                    header(function),
                    [0; 16]
                )),
                "function {function}"
            );
        }
        // Too short for the four fields Options+ requires.
        assert!(!FnInversionMultiHostFeature::is_state_broadcast(
            &Message::Short(header(0), [1, 1, 0])
        ));
    }

    #[test]
    fn single_host_state_broadcast_is_function_one() {
        use super::FnInversionWithDefaultStateFeature;

        assert!(FnInversionWithDefaultStateFeature::is_state_broadcast(1));
        for function in [0, 2, 0x0f] {
            assert!(
                !FnInversionWithDefaultStateFeature::is_state_broadcast(function),
                "function {function}"
            );
        }
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
