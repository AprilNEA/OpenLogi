//! Battery presentation and warning policy, shared by the device UI and tray.

use crate::device::{BatteryFreshness, BatteryInfo, BatteryLevel, BatteryStatus};

/// The current warning level, ordered so the most urgent device wins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// No current low-battery condition.
    #[default]
    Normal,
    /// At most twenty percent remaining.
    Low,
    /// At most ten percent remaining.
    Critical,
}

impl BatteryInfo {
    /// Whether power is actively charging the device.
    #[must_use]
    pub fn is_charging(&self) -> bool {
        matches!(
            self.status,
            BatteryStatus::Charging | BatteryStatus::ChargingSlow
        )
    }

    /// A current, valid percentage. Charging zero and unknown zero are firmware
    /// sentinels, not evidence of an empty battery.
    #[must_use]
    pub fn usable_percentage(&self) -> Option<u8> {
        (self.freshness == BatteryFreshness::Current
            && self.percentage <= 100
            && !(self.percentage == 0
                && (self.is_charging() || self.level == BatteryLevel::Unknown)))
            .then_some(self.percentage)
    }

    /// Warning state from a current reading; external power suppresses warnings.
    #[must_use]
    pub fn severity(&self) -> Severity {
        if self.is_charging() || self.status == BatteryStatus::Full {
            return Severity::Normal;
        }
        match self.usable_percentage() {
            Some(0..=10) => Severity::Critical,
            Some(11..=20) => Severity::Low,
            _ => Severity::Normal,
        }
    }

    /// Whether this reading warrants the low-battery visual treatment.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.severity() != Severity::Normal
    }

    /// A measured recovery rearms notifications. Merely plugging in does not.
    #[must_use]
    pub fn rearms_warning(&self) -> bool {
        self.usable_percentage()
            .is_some_and(|percentage| percentage > 25)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_policy_uses_current_percentage_not_firmware_bucket() {
        for (percentage, expected) in [
            (0, Severity::Critical),
            (10, Severity::Critical),
            (11, Severity::Low),
            (20, Severity::Low),
            (21, Severity::Normal),
            (100, Severity::Normal),
            (255, Severity::Normal),
        ] {
            for level in [
                BatteryLevel::Good,
                BatteryLevel::Low,
                BatteryLevel::Critical,
            ] {
                let mut battery = BatteryInfo {
                    percentage,
                    level,
                    status: BatteryStatus::Discharging,
                    freshness: BatteryFreshness::Current,
                };
                assert_eq!(battery.severity(), expected);
                for status in [
                    BatteryStatus::Charging,
                    BatteryStatus::ChargingSlow,
                    BatteryStatus::Full,
                ] {
                    battery.status = status;
                    assert_eq!(battery.severity(), Severity::Normal);
                }
                battery.status = BatteryStatus::Discharging;
                battery.freshness = BatteryFreshness::Cached;
                assert_eq!(battery.usable_percentage(), None);
                assert_eq!(battery.severity(), Severity::Normal);
                assert!(!battery.rearms_warning());
            }
        }
    }

    #[test]
    fn unknown_zero_is_not_an_empty_battery_and_recovery_has_hysteresis() {
        let mut battery = BatteryInfo {
            percentage: 0,
            level: BatteryLevel::Unknown,
            status: BatteryStatus::Discharging,
            freshness: BatteryFreshness::Current,
        };
        assert_eq!(battery.usable_percentage(), None);
        for (percentage, rearms) in [(20, false), (21, false), (25, false), (26, true)] {
            battery.percentage = percentage;
            assert_eq!(battery.rearms_warning(), rearms);
        }
    }
}
