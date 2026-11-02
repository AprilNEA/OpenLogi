//! Implements the `SmartShift` feature (ID `0x2110`) that allows controlling a
//! smart shift enhanced scroll wheel.

use std::hash::Hash;
use std::num::NonZeroU8;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Implements the `SmartShift` / `0x2110` feature.
#[derive(Feature)]
#[creatable(id = 0x2110, version = 0)]
pub struct SmartShiftFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl SmartShiftFeature {
    /// Retrieves the current ratchet control mode.
    ///
    /// [`RatchetControlMode::wheel_mode`] will only reflect the value set
    /// either by software or the wheel mode button. It will not provide
    /// information about whether the wheel is in auto-disengaged mode.
    pub async fn get_ratchet_control_mode(&self) -> Result<RatchetControlMode, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        RatchetControlMode::from_payload(&payload)
    }

    /// Sets the ratchet control mode and returns the mode the device reports
    /// after the write.
    ///
    /// For `auto_disengage` (and `auto_disengage_default` respectively), the
    /// values `0x01..=0xfe` correspond to the amount of quarter-turns the wheel
    /// has to make per second for the wheel to disengage.
    /// `0xff` enables permanent ratchet mode.
    ///
    /// All values are optional and will stay as they are if provided with
    /// [`None`] — encoded as the wire's `0` sentinel, so a written value is
    /// non-zero by construction rather than by a doc warning.
    ///
    /// The reply has the getter's layout. Logi Options+ hands its first two
    /// bytes, the wheel mode and the auto-disengage threshold, back to the
    /// caller (reverse-engineered from its `SetRatchetControlMode`), as the
    /// `0x2111` setter here returns its status. A reply is not proof that
    /// the write took: callers compare the fields they wrote.
    pub async fn set_ratchet_control_mode(
        &self,
        wheel_mode: Option<WheelMode>,
        auto_disengage: Option<NonZeroU8>,
        auto_disengage_default: Option<NonZeroU8>,
    ) -> Result<RatchetControlMode, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(
                1,
                [
                    wheel_mode.map_or(0, u8::from),
                    auto_disengage.map_or(0, NonZeroU8::get),
                    auto_disengage_default.map_or(0, NonZeroU8::get),
                ],
            )
            .await?
            .extend_payload();
        RatchetControlMode::from_payload(&payload)
    }
}

/// Represents the ratchet control mode of the mouse wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct RatchetControlMode {
    /// The mode the wheel is currently set to.
    ///
    /// This does not reflect the automatic disengage state.
    pub wheel_mode: WheelMode,

    /// The amount of quarter-turns per second it takes for the wheel to
    /// automatically disengage.
    ///
    /// If this value is `0xff`, the wheel will not disengage automatically.
    pub auto_disengage: u8,

    /// The default value of [`Self::auto_disengage`].
    pub auto_disengage_default: u8,
}

impl RatchetControlMode {
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            wheel_mode: WheelMode::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            auto_disengage: payload[1],
            auto_disengage_default: payload[2],
        })
    }
}

/// Represents the ratchet mode of the scroll wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WheelMode {
    /// Free-spin wheel mode.
    Freespin = 1,
    /// Ratchet wheel mode.
    Ratchet = 2,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU8;
    use std::sync::Arc;

    use super::{Hidpp20Error, RatchetControlMode, SmartShiftFeature, WheelMode};
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    /// What `set_ratchet_control_mode(Freespin, 10, None)` returns when the
    /// device answers with `reply`.
    fn set_with_reply(reply: [u8; 3]) -> Result<RatchetControlMode, Hidpp20Error> {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = SmartShiftFeature::new(channel, 3, 6);
            let mut write = Box::pin(feature.set_ratchet_control_mode(
                Some(WheelMode::Freespin),
                NonZeroU8::new(10),
                None,
            ));
            assert!(futures::poll!(write.as_mut()).is_pending());
            let header = MessageHeader {
                device_index: 3,
                feature_index: 6,
                function_id: U4::from_lo(1),
                software_id: U4::from_lo(1),
            };
            handle
                .send_incoming(Message::Short(header, reply).into())
                .await;
            let result = write.await;
            let written = handle.written_reports();
            let request = written.last().expect("setRatchetControlMode was sent");
            assert_eq!(&request[4..7], &[1, 10, 0]);
            result
        })
    }

    #[test]
    fn set_returns_the_mode_the_device_reports() {
        // The device kept ratchet mode and its own threshold.
        let applied = set_with_reply([2, 0x20, 0x0a]).unwrap();
        assert_eq!(applied.wheel_mode, WheelMode::Ratchet);
        assert_eq!(
            (applied.auto_disengage, applied.auto_disengage_default),
            (0x20, 0x0a)
        );
        assert!(matches!(
            set_with_reply([0, 10, 0]),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }
}
