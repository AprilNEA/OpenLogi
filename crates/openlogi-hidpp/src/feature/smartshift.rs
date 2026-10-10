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

        Ok(RatchetControlMode {
            wheel_mode: WheelMode::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            auto_disengage: payload[1],
            auto_disengage_default: payload[2],
        })
    }

    /// Sets the ratchet control mode.
    ///
    /// For `auto_disengage` (and `auto_disengage_default` respectively), the
    /// values `0x01..=0xfe` correspond to the amount of quarter-turns the wheel
    /// has to make per second for the wheel to disengage.
    /// `0xff` enables permanent ratchet mode.
    ///
    /// All values are optional and will stay as they are if provided with
    /// [`None`] — encoded as the wire's `0` sentinel, so a written value is
    /// non-zero by construction rather than by a doc warning.
    pub async fn set_ratchet_control_mode(
        &self,
        wheel_mode: Option<WheelMode>,
        auto_disengage: Option<NonZeroU8>,
        auto_disengage_default: Option<NonZeroU8>,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(
                1,
                [
                    wheel_mode.map_or(0, u8::from),
                    auto_disengage.map_or(0, NonZeroU8::get),
                    auto_disengage_default.map_or(0, NonZeroU8::get),
                ],
            )
            .await?;

        Ok(())
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
