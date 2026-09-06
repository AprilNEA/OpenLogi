//! Implements `CenturionBatterySoc` (feature `0x0104`) for gaming headsets (e.g. PRO X 2 LIGHTSPEED).

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Battery charging status reported by Centurion devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenturionChargingStatus {
    /// Discharging battery.
    Discharging,
    /// Recharging battery.
    Recharging,
    /// Fully charged battery.
    Full,
}

/// Battery state of charge reading from feature `0x0104`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CenturionBatteryInfo {
    /// Battery charge percentage (0-100).
    pub percentage: u8,
    /// Charging status.
    pub status: CenturionChargingStatus,
}

/// Implements the `CenturionBatterySoc` / `0x0104` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x0104, version = 0)]
pub struct CenturionBatterySocFeature {
    endpoint: FeatureEndpoint,
}

impl CenturionBatterySocFeature {
    /// Reads battery percentage and charging status.
    /// Function 0 reply format: [percentage, duplicate_percentage, status, ...]
    pub async fn get_battery_info(&self) -> Result<CenturionBatteryInfo, Hidpp20Error> {
        let reply = self.endpoint.call(0, [0; 3]).await?;
        let payload = reply.extend_payload();

        let percentage = payload.first().copied().unwrap_or(0);
        let charging_byte = payload.get(2).copied().unwrap_or(0);

        let status = match charging_byte {
            1 | 2 => CenturionChargingStatus::Recharging,
            3 => CenturionChargingStatus::Full,
            _ => CenturionChargingStatus::Discharging,
        };

        Ok(CenturionBatteryInfo {
            percentage,
            status,
        })
    }
}
