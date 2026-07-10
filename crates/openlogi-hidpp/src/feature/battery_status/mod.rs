//! Implements the legacy `BatteryStatus` feature (ID `0x1000`).

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Legacy HID++ 2.0 battery status feature used by devices such as the G305.
#[derive(Clone)]
pub struct BatteryStatusFeature {
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for BatteryStatusFeature {
    const ID: u16 = 0x1000;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for BatteryStatusFeature {}

impl BatteryStatusFeature {
    /// Reads the current percentage and charge status.
    pub async fn get_battery_level_status(&self) -> Result<BatteryInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(BatteryInfo {
            percentage: payload[0],
            next_level: payload[1],
            status: BatteryStatus::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
        })
    }
}

/// Legacy battery information returned by function 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct BatteryInfo {
    /// Current charge percentage.
    pub percentage: u8,
    /// Percentage threshold for the next coarser battery level.
    pub next_level: u8,
    /// Charging/discharging state.
    pub status: BatteryStatus,
}

/// Charge state used by the legacy battery feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum BatteryStatus {
    /// The battery is discharging.
    Discharging = 0,
    /// The battery is charging.
    Charging = 1,
    /// Charging is in its final stage.
    ChargingNearlyFull = 2,
    /// Charging is complete.
    Full = 3,
    /// Charging is slower than optimal.
    ChargingSlow = 4,
    /// The reported battery type is invalid.
    InvalidBattery = 5,
    /// Charging stopped because of a thermal condition.
    ThermalError = 6,
    /// Another charging error occurred.
    ChargingError = 7,
}
