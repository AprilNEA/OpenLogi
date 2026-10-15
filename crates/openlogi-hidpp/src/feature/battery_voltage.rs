//! Implements the `BatteryVoltage` feature (ID `0x1001`) that reports a
//! device's battery charge as a measured voltage plus a charging-flags byte.
//!
//! G-series wireless gaming devices (G915, G903 LS, G502 LIGHTSPEED) expose
//! `0x1001` and neither the legacy `0x1000` nor the unified `0x1004`, so
//! without this feature the inventory probe finds no battery source for them
//! at all. Unlike its siblings the feature reports no percentage — callers
//! estimate one from the voltage (see `openlogi-hid`'s mappings).
//!
//! Only `getBatteryInfo` (function `0`) is implemented. The device also
//! broadcasts event `0` when its voltage or charging state changes;
//! [`BatteryVoltageFeature::is_status_broadcast`] recognises it so a listener
//! can re-read the battery instead of waiting for a poll — the same scope
//! `BatteryStatus` (`0x1000`) keeps.
//!
//! The wire layout is not in a public Logitech spec: the voltage as a
//! big-endian millivolt `u16` followed by one flags byte was
//! reverse-engineered, and the decoding here follows Solaar
//! (`decipher_battery_voltage`) and libratbag's consensus on the flag bits.
//! The charge status values follow the Linux HID++ driver
//! (`hidpp20_battery_map_status_voltage`), which reads three status bits and
//! leaves values above `2` unknown.

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Implements the `BatteryVoltage` / `0x1001` feature.
#[derive(Feature)]
#[creatable(id = 0x1001, version = 0)]
pub struct BatteryVoltageFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl BatteryVoltageFeature {
    /// Whether an unsolicited report on this feature's index is its
    /// battery-voltage broadcast (event `0`; the Linux driver's
    /// `EVENT_BATTERY_VOLTAGE_STATUS_BROADCAST`).
    ///
    /// Deliberately independent of the payload, like
    /// `BatteryStatusFeature::is_status_broadcast`: a listener that only needs
    /// to know *that* the reading changed re-reads it with
    /// [`Self::get_battery_info`].
    #[must_use]
    pub const fn is_status_broadcast(function_id: u8) -> bool {
        function_id == 0
    }

    /// Reads the measured battery voltage and charging state (function `0`,
    /// `getBatteryInfo`).
    pub async fn get_battery_info(&self) -> Result<VoltageBatteryInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(VoltageBatteryInfo::from_wire(&payload))
    }
}

/// A reading from the `0x1001` `getBatteryInfo` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct VoltageBatteryInfo {
    /// Measured battery voltage in millivolt — roughly `3500` (empty) to
    /// `4200` (full) for the single-cell Li-Po batteries these devices carry.
    pub voltage_mv: u16,

    /// The charging state decoded from the flags byte.
    pub status: VoltageChargingStatus,

    /// The firmware's "charge level critical" marker (flags bit `5`).
    pub critical: bool,
}

impl VoltageBatteryInfo {
    /// Decodes a `getBatteryInfo` response payload: voltage as a big-endian
    /// millivolt `u16` in bytes `0`–`1`, the charging flags in byte `2`.
    #[must_use]
    pub fn from_wire(payload: &[u8; 16]) -> Self {
        let flags = payload[2];
        Self {
            voltage_mv: u16::from_be_bytes([payload[0], payload[1]]),
            status: VoltageChargingStatus::from_flags(flags),
            critical: flags & (1 << 5) != 0,
        }
    }
}

/// Charging state decoded from the `0x1001` flags byte.
///
/// Bit `7` set means external power is present; bits `0`–`2` then carry the
/// charge status (`0` charging, `1` charge complete, `2` charge fault, any
/// other value undefined) and bits `3` / `4` mark fast / slow charging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum VoltageChargingStatus {
    /// Running on battery (bit `7` clear).
    Discharging,
    /// Charging at the standard rate.
    Charging,
    /// Charging at a raised current (bit `3`).
    ChargingFast,
    /// Charging at reduced current (bit `4`).
    ChargingSlow,
    /// On external power with charge complete (status bits `0b01`).
    Full,
    /// On external power but not charging — a charge fault (status bits
    /// `0b10`).
    NotCharging,
    /// On external power with a status value the protocol leaves undefined
    /// (bits `0`–`2` equal to `3` through `7`).
    Unknown,
}

impl VoltageChargingStatus {
    /// Decodes the flags byte. Total on purpose, so a battery reading never
    /// vanishes over an unknown bit: an undefined status value decodes to
    /// [`Self::Unknown`] and the voltage still reads. It does not borrow a
    /// neighbouring bucket — that would show `0b011` as a full battery.
    fn from_flags(flags: u8) -> Self {
        if flags & (1 << 7) == 0 {
            return Self::Discharging;
        }
        match flags & 0x07 {
            0x00 => {
                if flags & (1 << 3) != 0 {
                    Self::ChargingFast
                } else if flags & (1 << 4) != 0 {
                    Self::ChargingSlow
                } else {
                    Self::Charging
                }
            }
            0x01 => Self::Full,
            0x02 => Self::NotCharging,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VoltageBatteryInfo, VoltageChargingStatus};

    /// Builds a 16-byte payload from the 3 meaningful bytes.
    fn payload(voltage_mv: u16, flags: u8) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[..2].copy_from_slice(&voltage_mv.to_be_bytes());
        payload[2] = flags;
        payload
    }

    #[test]
    fn discharging_reading_decodes_voltage_and_status() {
        let info = VoltageBatteryInfo::from_wire(&payload(3781, 0x00));
        assert_eq!(info.voltage_mv, 3781);
        assert_eq!(info.status, VoltageChargingStatus::Discharging);
        assert!(!info.critical);
    }

    #[test]
    fn external_power_flag_alone_means_standard_charging() {
        let info = VoltageBatteryInfo::from_wire(&payload(4100, 0x80));
        assert_eq!(info.status, VoltageChargingStatus::Charging);
    }

    #[test]
    fn charge_status_bits_take_precedence_over_rate_bits() {
        // Charge complete wins over a stale fast-charge bit.
        let info = VoltageBatteryInfo::from_wire(&payload(4186, 0x80 | 0x08 | 0x01));
        assert_eq!(info.status, VoltageChargingStatus::Full);
        let info = VoltageBatteryInfo::from_wire(&payload(4000, 0x80 | 0x02));
        assert_eq!(info.status, VoltageChargingStatus::NotCharging);
    }

    #[test]
    fn rate_bits_split_fast_and_slow_charging() {
        let fast = VoltageBatteryInfo::from_wire(&payload(3900, 0x80 | 0x08));
        assert_eq!(fast.status, VoltageChargingStatus::ChargingFast);
        let slow = VoltageBatteryInfo::from_wire(&payload(3900, 0x80 | 0x10));
        assert_eq!(slow.status, VoltageChargingStatus::ChargingSlow);
    }

    #[test]
    fn critical_bit_is_surfaced_independently_of_status() {
        let info = VoltageBatteryInfo::from_wire(&payload(3520, 0x20));
        assert_eq!(info.status, VoltageChargingStatus::Discharging);
        assert!(info.critical);
    }

    #[test]
    fn without_external_power_the_rate_bits_are_meaningless() {
        // Bit 7 clear: whatever the low bits claim, the device runs on battery.
        let info = VoltageBatteryInfo::from_wire(&payload(3700, 0x1b));
        assert_eq!(info.status, VoltageChargingStatus::Discharging);
    }

    #[test]
    fn on_external_power_only_status_values_0_to_2_are_defined() {
        let cases = [
            (0x80, VoltageChargingStatus::Charging),
            (0x81, VoltageChargingStatus::Full),
            (0x82, VoltageChargingStatus::NotCharging),
            (0x83, VoltageChargingStatus::Unknown),
            (0x84, VoltageChargingStatus::Unknown),
            (0x85, VoltageChargingStatus::Unknown),
            (0x86, VoltageChargingStatus::Unknown),
            (0x87, VoltageChargingStatus::Unknown),
        ];
        for (flags, expected) in cases {
            let info = VoltageBatteryInfo::from_wire(&payload(4000, flags));
            assert_eq!(info.status, expected, "flags {flags:#04x}");
            assert_eq!(info.voltage_mv, 4000, "flags {flags:#04x}");
        }
    }

    #[test]
    fn rate_bits_do_not_turn_an_undefined_status_into_charging() {
        // Status 4 with the fast-charge bit set is still an undefined status.
        let info = VoltageBatteryInfo::from_wire(&payload(3900, 0x80 | 0x08 | 0x04));
        assert_eq!(info.status, VoltageChargingStatus::Unknown);
    }
}
