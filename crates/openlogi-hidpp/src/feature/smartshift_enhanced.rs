//! Implements `SmartShiftWheelEnhanced` (feature `0x2111`).

use std::num::NonZeroU8;

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, smartshift::WheelMode},
    protocol::v20::{self, Hidpp20Error},
};

bitflags::bitflags! {
    /// Capabilities reported by `SmartShiftWheelEnhanced`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct SmartShiftEnhancedCapabilities: u8 {
        /// The device supports tunable ratchet torque.
        const TUNABLE_TORQUE = 1 << 0;
    }
}

/// Capability and default values for enhanced SmartShift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SmartShiftEnhancedInfo {
    /// Supported capabilities.
    pub capabilities: SmartShiftEnhancedCapabilities,
    /// Default automatic disengage threshold.
    pub auto_disengage_default: u8,
    /// Default tunable torque, as a percentage of maximum force.
    pub default_tunable_torque: u8,
    /// Maximum force in gram-force units.
    pub max_force: u8,
}

/// Current enhanced SmartShift status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct SmartShiftEnhancedStatus {
    /// Current requested wheel mode.
    pub wheel_mode: WheelMode,
    /// Automatic disengage threshold.
    pub auto_disengage: u8,
    /// Current tunable torque, as a percentage of maximum force.
    pub current_tunable_torque: u8,
}

/// A tunable-torque level to write, in percent of the wheel's maximum force.
///
/// Only `1..=100` is a level. Zero is the write-only "do not change"
/// sentinel, which [`SmartShiftEnhancedStatusChange`] spells as `None`, and a
/// byte above `100` is not a percentage. Solaar bounds its torque setting to
/// the same range, and Logi Options+ clamps the level to it before a write
/// (reverse-engineered).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TorquePercent(u8);

impl TorquePercent {
    /// The lowest level, `1`.
    pub const MIN: Self = Self(1);
    /// The highest level, `100`.
    pub const MAX: Self = Self(100);

    /// Validates a level, or `None` outside `1..=100`.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= Self::MIN.0 && value <= Self::MAX.0 {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The level sent on the wire.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Enhanced SmartShift status update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SmartShiftEnhancedStatusChange {
    /// Wheel mode to apply, or `None` to leave unchanged.
    pub wheel_mode: Option<WheelMode>,
    /// Automatic disengage threshold, or `None` to leave unchanged.
    ///
    /// HID++ encodes `0` as “do not change”, so writable values must be non-zero.
    pub auto_disengage: Option<NonZeroU8>,
    /// Tunable torque, or `None` to leave unchanged.
    ///
    /// HID++ encodes `0` as “do not change”, so a written level is a
    /// [`TorquePercent`].
    pub tunable_torque: Option<TorquePercent>,
}

/// Implements the `SmartShiftWheelEnhanced` / `0x2111` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x2111, version = 0)]
pub struct SmartShiftEnhancedFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl SmartShiftEnhancedFeature {
    /// Retrieves enhanced SmartShift capabilities and defaults.
    ///
    /// The four fields need more bytes than a short reply holds, so the
    /// reply must be long: a short one would zero-fill the maximum force.
    /// Logi Options+ also refuses fewer than four bytes (reverse-engineered
    /// from its `getCapabilities`). The status functions read three bytes,
    /// which every reply carries.
    pub async fn get_capabilities(&self) -> Result<SmartShiftEnhancedInfo, Hidpp20Error> {
        let v20::Message::Long(_, payload) = self.endpoint.call(0, [0; 3]).await? else {
            return Err(Hidpp20Error::UnsupportedResponse);
        };
        Ok(SmartShiftEnhancedInfo {
            capabilities: SmartShiftEnhancedCapabilities::from_bits_retain(payload[0]),
            auto_disengage_default: payload[1],
            default_tunable_torque: payload[2],
            max_force: payload[3],
        })
    }

    /// Retrieves the current enhanced SmartShift ratchet control mode.
    pub async fn get_ratchet_control_mode(&self) -> Result<SmartShiftEnhancedStatus, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        SmartShiftEnhancedStatus::from_payload(payload)
    }

    /// Sets selected enhanced SmartShift fields and returns the resulting status.
    ///
    /// A `None` field is encoded as `0`, the documented “do not change” value.
    pub async fn set_ratchet_control_mode(
        &self,
        change: SmartShiftEnhancedStatusChange,
    ) -> Result<SmartShiftEnhancedStatus, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(
                2,
                [
                    change.wheel_mode.map_or(0, u8::from),
                    change.auto_disengage.map_or(0, NonZeroU8::get),
                    change.tunable_torque.map_or(0, TorquePercent::get),
                ],
            )
            .await?
            .extend_payload();
        SmartShiftEnhancedStatus::from_payload(payload)
    }
}

impl SmartShiftEnhancedStatus {
    fn from_payload(payload: [u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            wheel_mode: WheelMode::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            auto_disengage: payload[1],
            current_tunable_torque: payload[2],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        Hidpp20Error, SmartShiftEnhancedCapabilities, SmartShiftEnhancedFeature,
        SmartShiftEnhancedStatus, SmartShiftEnhancedStatusChange, TorquePercent, WheelMode,
    };
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    /// Runs `call` on a feature at device 3, index 6 whose device answers
    /// `function` with `reply`, and returns the result and the request sent.
    fn exchange<T, F>(
        function: u8,
        call: impl FnOnce(SmartShiftEnhancedFeature) -> F,
        reply: impl FnOnce(MessageHeader) -> Message,
    ) -> (Result<T, Hidpp20Error>, Vec<u8>)
    where
        F: Future<Output = Result<T, Hidpp20Error>>,
    {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let mut pending = Box::pin(call(SmartShiftEnhancedFeature::new(channel, 3, 6)));
            assert!(futures::poll!(pending.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 6,
                function_id: U4::from_lo(function),
                software_id: U4::from_lo(1),
            };
            handle.send_incoming(reply(header).into()).await;
            let result = pending.await;
            let written = handle.written_reports();
            let request = written.last().expect("the request was sent").clone();
            (result, request)
        })
    }

    #[test]
    fn parses_status() {
        let mut payload = [0; 16];
        payload[0] = 2;
        payload[1] = 0xff;
        payload[2] = 33;

        let status = SmartShiftEnhancedStatus::from_payload(payload).unwrap();

        assert_eq!(status.wheel_mode, WheelMode::Ratchet);
        assert_eq!(status.auto_disengage, 0xff);
        assert_eq!(status.current_tunable_torque, 33);
    }

    #[test]
    fn unknown_wheel_mode_is_an_unsupported_response() {
        let mut payload = [0; 16];
        payload[0] = 9;

        let err = SmartShiftEnhancedStatus::from_payload(payload).unwrap_err();

        assert!(matches!(err, Hidpp20Error::UnsupportedResponse));
    }

    #[test]
    fn a_torque_percent_is_exactly_one_to_one_hundred() {
        // Zero is the "do not change" sentinel and 101 the first byte that
        // is not a percentage: neither is a level a caller can write.
        assert_eq!(TorquePercent::new(0), None);
        assert_eq!(TorquePercent::new(101), None);
        for value in 0..=u8::MAX {
            assert_eq!(
                TorquePercent::new(value).map(TorquePercent::get),
                (1..=100).contains(&value).then_some(value),
                "torque byte {value}"
            );
        }
        assert_eq!(
            (TorquePercent::MIN.get(), TorquePercent::MAX.get()),
            (1, 100)
        );
    }

    #[test]
    fn set_writes_the_torque_level_or_the_do_not_change_sentinel() {
        for (torque, byte) in [
            (None, 0),
            (Some(TorquePercent::MIN), 1),
            (Some(TorquePercent::MAX), 100),
        ] {
            let change = SmartShiftEnhancedStatusChange {
                tunable_torque: torque,
                ..SmartShiftEnhancedStatusChange::default()
            };
            let (status, request) = exchange(
                2,
                |feature| async move { feature.set_ratchet_control_mode(change).await },
                |header| Message::Short(header, [2, 0xff, 50]),
            );
            assert_eq!(&request[4..7], &[0, 0, byte], "torque {torque:?}");
            assert_eq!(status.unwrap().current_tunable_torque, 50);
        }
    }

    #[test]
    fn get_capabilities_refuses_a_short_reply() {
        let get =
            |feature: SmartShiftEnhancedFeature| async move { feature.get_capabilities().await };
        // Three of the four fields: zero-filled, this would read as a wheel
        // whose maximum force is zero.
        let (short, _) = exchange(0, get, |header| Message::Short(header, [0x01, 0x1e, 50]));
        assert!(matches!(short, Err(Hidpp20Error::UnsupportedResponse)));

        let mut long = [0; 16];
        long[..4].copy_from_slice(&[0x01, 0x1e, 50, 0x40]);
        let (info, _) = exchange(0, get, |header| Message::Long(header, long));
        let info = info.unwrap();
        assert_eq!(
            info.capabilities,
            SmartShiftEnhancedCapabilities::TUNABLE_TORQUE
        );
        assert_eq!(
            (
                info.auto_disengage_default,
                info.default_tunable_torque,
                info.max_force
            ),
            (0x1e, 50, 0x40)
        );
    }

    #[test]
    fn get_status_reads_a_short_reply() {
        // The status is three bytes, so a short reply carries all of it.
        let (status, _) = exchange(
            1,
            |feature| async move { feature.get_ratchet_control_mode().await },
            |header| Message::Short(header, [1, 0x20, 75]),
        );
        let status = status.unwrap();
        assert_eq!(
            (
                status.wheel_mode,
                status.auto_disengage,
                status.current_tunable_torque
            ),
            (WheelMode::Freespin, 0x20, 75)
        );
    }
}
