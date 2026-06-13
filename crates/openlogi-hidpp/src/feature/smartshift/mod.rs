//! Implements the `SmartShift` feature (ID `0x2110`) that allows controlling a
//! smart shift enhanced scroll wheel.

use std::{hash::Hash, sync::Arc};

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements the `SmartShift` / `0x2110` feature.
pub struct SmartShiftFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for SmartShiftFeature {
    const ID: u16 = 0x2110;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for SmartShiftFeature {}

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
    /// [`None`].
    ///
    /// For `auto_disengage` and `auto_disengange_default`, `0` will have the
    /// same effect as [`None`].
    pub async fn set_ratchet_control_mode(
        &self,
        wheel_mode: Option<WheelMode>,
        auto_disengage: Option<u8>,
        auto_disengage_default: Option<u8>,
    ) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(
                1,
                [
                    wheel_mode.map_or(0, u8::from),
                    auto_disengage.unwrap_or(0),
                    auto_disengage_default.unwrap_or(0),
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
    Freespin = 1,
    Ratchet = 2,
}
