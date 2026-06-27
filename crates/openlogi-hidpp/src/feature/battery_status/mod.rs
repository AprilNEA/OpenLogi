//! Implements the legacy `BatteryStatus` feature (ID `0x1000`) that reports a
//! device's battery charge as a discharge level plus a charging status.
//!
//! This is the predecessor of `UnifiedBattery` (`0x1004`): older mice such as
//! the MX Master 2S expose `0x1000` and never `0x1004`, so the inventory probe
//! falls back to this feature when the unified one is absent — the same
//! enhanced-then-legacy pattern `SmartShift` uses for `0x2111` / `0x2110`.
//!
//! Only `getBatteryLevelStatus` (function `0`) is implemented; the optional
//! `getBatteryCapability` (function `1`) and the broadcast event aren't needed
//! to display a charge reading.

use std::{hash::Hash, sync::Arc};

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements the legacy `BatteryStatus` / `0x1000` feature.
pub struct BatteryStatusFeature {
    /// The endpoint this feature talks to.
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
    /// Reads the current battery level and charging status (function `0`,
    /// `getBatteryLevelStatus`).
    pub async fn get_battery_level_status(&self) -> Result<LegacyBatteryInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();

        Ok(LegacyBatteryInfo {
            discharge_level: payload[0],
            next_level: payload[1],
            status: LegacyBatteryStatus::try_from(payload[2])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
        })
    }
}

/// A reading from the legacy `0x1000` `getBatteryLevelStatus` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct LegacyBatteryInfo {
    /// Current battery charge as a percentage (`0`–`100`). Logitech firmware
    /// reports this in coarse steps rather than a continuous value.
    pub discharge_level: u8,

    /// The next lower discharge step the firmware will report — a hint at the
    /// reporting granularity. Unused for display.
    pub next_level: u8,

    /// The current charging status.
    pub status: LegacyBatteryStatus,
}

/// Charging status reported by the legacy `0x1000` feature. Values follow the
/// HID++ `batteryStatus` enumeration (see Solaar / `hid-logitech-hidpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum LegacyBatteryStatus {
    Discharging = 0,
    Recharging = 1,
    AlmostFull = 2,
    Full = 3,
    SlowRecharge = 4,
    InvalidBattery = 5,
    ThermalError = 6,
    /// "Other charging error" (Solaar lists value 7). Kept explicit so a device
    /// reporting it surfaces as Unknown instead of failing the parse and making
    /// the battery indicator vanish from the UI.
    Other = 7,
}
