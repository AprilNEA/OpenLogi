//! Bare-minimum HID++ `BatteryStatus` (feature `0x1000`) wrapper.
//!
//! This is the legacy battery feature carried by older MX-class mice — the
//! MX Master 3, for instance, exposes `0x1000` rather than the newer
//! `0x1004 UnifiedBattery` that `hidpp 0.2` ships a typed wrapper for. We
//! implement only `getBatteryLevelStatus` (function `0x0`), enough to surface
//! a charge percentage + charging state.
//!
//! Follows the same shape as the typed wrappers `hidpp` ships
//! (`UnifiedBatteryFeatureV0`, …) and the sibling `AdjustableDpiFeatureV0`.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// Charging state reported by `0x1000`'s `batteryStatus` byte. Mirrors the
/// HID++ enumeration; unknown/future codes fall through to [`Self::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryChargeState {
    Discharging,
    Recharging,
    /// Charging, in the final (top-off) stage.
    AlmostFull,
    /// Charge complete — battery full.
    Full,
    /// Recharging below optimal speed (e.g. weak USB port).
    SlowRecharge,
    /// Invalid battery / thermal / other charging error.
    Error,
    Unknown,
}

impl BatteryChargeState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Discharging,
            1 => Self::Recharging,
            2 => Self::AlmostFull,
            3 => Self::Full,
            4 => Self::SlowRecharge,
            5..=7 => Self::Error,
            _ => Self::Unknown,
        }
    }
}

/// One reading of `getBatteryLevelStatus`.
#[derive(Debug, Clone, Copy)]
pub struct BatteryLevelStatus {
    /// Current charge as a percentage (0–100).
    pub level_percent: u8,
    /// Charging / discharging state.
    pub charge_state: BatteryChargeState,
}

/// `BatteryStatus` / `0x1000` feature, version 0+.
#[derive(Clone)]
pub struct BatteryStatusFeatureV0 {
    chan: Arc<HidppChannel>,
    device_index: u8,
    feature_index: u8,
}

impl CreatableFeature for BatteryStatusFeatureV0 {
    const ID: u16 = 0x1000;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }
}

impl Feature for BatteryStatusFeatureV0 {}

impl BatteryStatusFeatureV0 {
    /// Read the current charge level + charging state (function `0x0`).
    /// Response payload is `[dischargeLevel, nextLevel, status, …]`.
    pub async fn get_battery_level_status(&self) -> Result<BatteryLevelStatus, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(0),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, 0x00],
            ))
            .await?;
        let payload = response.extend_payload();
        Ok(BatteryLevelStatus {
            level_percent: payload[0],
            charge_state: BatteryChargeState::from_raw(payload[2]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_state_maps_known_codes_and_falls_back() {
        use BatteryChargeState::{
            AlmostFull, Discharging, Error, Full, Recharging, SlowRecharge, Unknown,
        };
        assert_eq!(BatteryChargeState::from_raw(0), Discharging);
        assert_eq!(BatteryChargeState::from_raw(1), Recharging);
        assert_eq!(BatteryChargeState::from_raw(2), AlmostFull);
        assert_eq!(BatteryChargeState::from_raw(3), Full);
        assert_eq!(BatteryChargeState::from_raw(4), SlowRecharge);
        for code in 5..=7 {
            assert_eq!(BatteryChargeState::from_raw(code), Error);
        }
        assert_eq!(BatteryChargeState::from_raw(99), Unknown);
    }
}
