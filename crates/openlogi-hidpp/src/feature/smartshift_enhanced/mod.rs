//! Implements `SmartShiftWheelEnhanced` (feature `0x2111`).

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint, smartshift::WheelMode},
    protocol::v20::Hidpp20Error,
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

/// Enhanced SmartShift status update.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SmartShiftEnhancedStatusChange {
    /// Wheel mode to apply, or `None` to leave unchanged.
    pub wheel_mode: Option<WheelMode>,
    /// Automatic disengage threshold, or `None` to leave unchanged.
    pub auto_disengage: Option<u8>,
    /// Tunable torque, or `None` to leave unchanged.
    pub tunable_torque: Option<u8>,
}

/// Implements the `SmartShiftWheelEnhanced` / `0x2111` feature.
#[derive(Clone)]
pub struct SmartShiftEnhancedFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for SmartShiftEnhancedFeature {
    const ID: u16 = 0x2111;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for SmartShiftEnhancedFeature {}

impl SmartShiftEnhancedFeature {
    /// Retrieves enhanced SmartShift capabilities and defaults.
    pub async fn get_capabilities(&self) -> Result<SmartShiftEnhancedInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
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
                    change.auto_disengage.unwrap_or(0),
                    change.tunable_torque.unwrap_or(0),
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
            wheel_mode: WheelMode::try_from(payload[0]).unwrap_or(WheelMode::Ratchet),
            auto_disengage: payload[1],
            current_tunable_torque: payload[2],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SmartShiftEnhancedStatus, WheelMode};

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
    fn falls_back_to_ratchet_for_unknown_wheel_mode() {
        let mut payload = [0; 16];
        payload[0] = 9;

        let status = SmartShiftEnhancedStatus::from_payload(payload).unwrap();

        assert_eq!(status.wheel_mode, WheelMode::Ratchet);
    }
}
