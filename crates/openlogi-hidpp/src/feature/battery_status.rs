//! Implements the legacy `BatteryStatus` feature (ID `0x1000`) that reports a
//! device's battery charge as a discharge level plus a charging status.
//!
//! This is the predecessor of `UnifiedBattery` (`0x1004`): older mice such as
//! the MX Master 2S expose `0x1000` and never `0x1004`, so the inventory probe
//! falls back to this feature when the unified one is absent — the same
//! enhanced-then-legacy pattern `SmartShift` uses for `0x2111` / `0x2110`.
//!
//! `getBatteryLevelStatus` (function `0`) reads the charge.
//! `getBatteryCapability` (function `1`) says how to read it: without mileage,
//! the charge is one of four coarse levels encoded as a percentage. The device
//! also broadcasts event `0` when its level or charging status changes;
//! [`BatteryStatusFeature::is_status_broadcast`] recognises it so a listener
//! can re-read the battery instead of waiting for a poll.

use std::hash::Hash;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Implements the legacy `BatteryStatus` / `0x1000` feature.
#[derive(Feature)]
#[creatable(id = 0x1000, version = 0)]
pub struct BatteryStatusFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl BatteryStatusFeature {
    /// Whether an unsolicited report on this feature's index is its
    /// battery-level broadcast (event `0`; the Linux driver's
    /// `EVENT_BATTERY_LEVEL_STATUS_BROADCAST`).
    ///
    /// Deliberately independent of the payload: the broadcast carries the same
    /// three bytes as [`Self::get_battery_level_status`], and a listener that
    /// only needs to know *that* the reading changed should re-read it rather
    /// than drop a broadcast whose status value this crate does not know.
    #[must_use]
    pub const fn is_status_broadcast(function_id: u8) -> bool {
        function_id == 0
    }

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

    /// Reads the device's static battery capability (function `1`,
    /// `getBatteryCapability`).
    ///
    /// Only bytes `0` and `1` are decoded, so a short reply is enough. In the
    /// HID++ 2.0 draft, bytes `2`–`4` carry a nominal battery life and a
    /// critical level that apply only with mileage. Nothing here reads them.
    pub async fn get_battery_capability(&self) -> Result<LegacyBatteryCapability, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        Ok(LegacyBatteryCapability::from_wire(&payload))
    }
}

/// A reading from the legacy `0x1000` `getBatteryLevelStatus` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct LegacyBatteryInfo {
    /// Current battery charge as a percentage (`0`–`100`). Logitech firmware
    /// reports this in coarse steps rather than a continuous value.
    /// [`LegacyBatteryCapability::reports_mileage`] tells whether it is a
    /// gauge reading or one of four coarse levels.
    ///
    /// A reading only while [`status`](Self::status) is
    /// [`LegacyBatteryStatus::Discharging`]. In every other state the firmware
    /// reports `0`, meaning unknown, as the Linux HID++ driver reads it.
    pub discharge_level: u8,

    /// The next lower discharge step the firmware will report — a hint at the
    /// reporting granularity. Unused for display.
    pub next_level: u8,

    /// The current charging status.
    pub status: LegacyBatteryStatus,
}

/// The static capability from the legacy `0x1000` `getBatteryCapability`
/// function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct LegacyBatteryCapability {
    /// How many discharge levels the device reports. The HID++ 2.0 draft
    /// allows `2`–`100`.
    pub number_of_levels: u8,

    /// The capability flags, with the bits the draft reserves kept as read.
    pub flags: LegacyBatteryCapabilityFlags,
}

impl LegacyBatteryCapability {
    /// Decodes bytes `0` and `1` of a `getBatteryCapability` reply.
    fn from_wire(payload: &[u8; 16]) -> Self {
        Self {
            number_of_levels: payload[0],
            flags: LegacyBatteryCapabilityFlags::from_bits_retain(payload[1]),
        }
    }

    /// Whether [`LegacyBatteryInfo::discharge_level`] is a mileage reading.
    ///
    /// That needs [`LegacyBatteryCapabilityFlags::MILEAGE`] and at least ten
    /// levels: the HID++ 2.0 draft has host software ignore the flag below ten.
    /// Otherwise the firmware reports one of four levels as a percentage, such
    /// as `25` for low and `50` for good. The Linux HID++ driver applies the
    /// same rule.
    #[must_use]
    pub const fn reports_mileage(&self) -> bool {
        self.number_of_levels >= 10 && self.flags.contains(LegacyBatteryCapabilityFlags::MILEAGE)
    }
}

bitflags::bitflags! {
    /// Byte `1` of the `getBatteryCapability` reply.
    ///
    /// Bit positions follow the HID++ 2.0 draft and the Linux HID++ driver.
    /// The draft requires bits `3`–`7` to be zero.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct LegacyBatteryCapabilityFlags: u8 {
        /// Host software should not show a battery on-screen display.
        const DISABLE_OSD = 1 << 0;
        /// The device calculates battery mileage. Ignored below ten levels
        /// (see [`LegacyBatteryCapability::reports_mileage`]).
        const MILEAGE = 1 << 1;
        /// The battery is rechargeable.
        const RECHARGEABLE = 1 << 2;
    }
}

/// Charging status reported by the legacy `0x1000` feature. Values follow the
/// HID++ `batteryStatus` enumeration (see Solaar / `hid-logitech-hidpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum LegacyBatteryStatus {
    /// Battery is discharging.
    Discharging = 0,
    /// Battery is recharging.
    Recharging = 1,
    /// Battery is charging and nearly full.
    AlmostFull = 2,
    /// Battery charge is complete.
    Full = 3,
    /// Battery is recharging below optimal speed.
    SlowRecharge = 4,
    /// The battery type is invalid.
    InvalidBattery = 5,
    /// The battery subsystem reported a thermal error.
    ThermalError = 6,
    /// "Other charging error" (Solaar lists value 7). Kept explicit so a device
    /// reporting it surfaces as Unknown instead of failing the parse and making
    /// the battery indicator vanish from the UI.
    Other = 7,
}

#[cfg(test)]
mod tests {
    use super::{LegacyBatteryCapability, LegacyBatteryCapabilityFlags};

    /// A `getBatteryCapability` reply payload: level count, then flags.
    fn capability(number_of_levels: u8, flags: u8) -> LegacyBatteryCapability {
        let mut payload = [0; 16];
        payload[0] = number_of_levels;
        payload[1] = flags;
        LegacyBatteryCapability::from_wire(&payload)
    }

    #[test]
    fn capability_decodes_the_level_count_and_each_flag() {
        let decoded = capability(4, 0b101);
        assert_eq!(decoded.number_of_levels, 4);
        assert_eq!(
            decoded.flags,
            LegacyBatteryCapabilityFlags::DISABLE_OSD | LegacyBatteryCapabilityFlags::RECHARGEABLE
        );
        assert_eq!(
            capability(100, 0b010).flags,
            LegacyBatteryCapabilityFlags::MILEAGE
        );
    }

    #[test]
    fn mileage_needs_the_flag_and_at_least_ten_levels() {
        for (levels, flags, mileage) in [
            (100, 0b010, true),
            (10, 0b010, true),
            // The draft has software ignore the flag below ten levels.
            (9, 0b010, false),
            (2, 0b110, false),
            (100, 0b100, false),
            (100, 0, false),
        ] {
            assert_eq!(
                capability(levels, flags).reports_mileage(),
                mileage,
                "{levels} levels, flags {flags:#04x}"
            );
        }
    }

    #[test]
    fn reserved_flag_bits_are_kept_and_do_not_hide_mileage() {
        let decoded = capability(20, 0xfa);
        assert_eq!(decoded.flags.bits(), 0xfa);
        assert!(decoded.reports_mileage());
    }
}
