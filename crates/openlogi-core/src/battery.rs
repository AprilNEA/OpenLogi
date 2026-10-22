//! Battery presentation and warning policies, shared by the UI, tray and overlay.

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
        self.percentage.filter(|&percentage| {
            self.freshness == BatteryFreshness::Current
                && percentage <= 100
                && !(percentage == 0 && (self.is_charging() || self.level == BatteryLevel::Unknown))
        })
    }

    /// Warning state from a current reading; external power suppresses warnings.
    #[must_use]
    pub fn severity(&self) -> Severity {
        if self.freshness != BatteryFreshness::Current
            || self.is_charging()
            || self.status == BatteryStatus::Full
        {
            return Severity::Normal;
        }
        match self.usable_percentage() {
            Some(0..=10) => Severity::Critical,
            Some(11..=20) => Severity::Low,
            None if self.percentage.is_none() => match self.level {
                BatteryLevel::Critical => Severity::Critical,
                BatteryLevel::Low => Severity::Low,
                _ => Severity::Normal,
            },
            _ => Severity::Normal,
        }
    }

    /// Whether this reading warrants the low-battery visual treatment.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.severity() != Severity::Normal
    }

    /// Whether the transient overlay may announce the next discharge.
    ///
    /// The overlay resets when charging starts, the charge reaches 30%, or a
    /// level-only device reports good/full again. Persistent OS alerts use
    /// [`Self::rearms_warning`] instead, so briefly plugging in cannot repeat them.
    #[must_use]
    pub fn rearms_overlay_notice(&self) -> bool {
        if self.freshness == BatteryFreshness::Cached {
            return false;
        }
        if self.is_charging() || self.status == BatteryStatus::Full {
            return true;
        }
        match self.usable_percentage() {
            Some(percentage) => percentage >= 30,
            None if self.percentage.is_none() => {
                matches!(self.level, BatteryLevel::Good | BatteryLevel::Full)
            }
            None => false,
        }
    }

    /// A measured recovery rearms persistent OS notifications. Merely plugging in does not.
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
                    percentage: Some(percentage),
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
    fn level_only_readings_keep_attention_without_inventing_percentages() {
        for (level, severity) in [
            (BatteryLevel::Critical, Severity::Critical),
            (BatteryLevel::Low, Severity::Low),
            (BatteryLevel::Good, Severity::Normal),
            (BatteryLevel::Unknown, Severity::Normal),
        ] {
            let mut battery = BatteryInfo {
                percentage: None,
                level,
                status: BatteryStatus::Discharging,
                freshness: BatteryFreshness::Current,
            };
            assert_eq!(battery.severity(), severity);
            assert_eq!(battery.usable_percentage(), None);
            assert!(!battery.rearms_warning());
            battery.freshness = BatteryFreshness::Cached;
            assert_eq!(battery.severity(), Severity::Normal);
            battery.freshness = BatteryFreshness::Current;
            battery.status = BatteryStatus::Charging;
            assert_eq!(battery.severity(), Severity::Normal);
        }
    }

    #[test]
    fn unknown_zero_is_not_an_empty_battery_and_recovery_has_hysteresis() {
        let mut battery = BatteryInfo {
            percentage: Some(0),
            level: BatteryLevel::Unknown,
            status: BatteryStatus::Discharging,
            freshness: BatteryFreshness::Current,
        };
        assert_eq!(battery.usable_percentage(), None);
        assert!(!battery.rearms_overlay_notice());
        battery.percentage = Some(255);
        assert!(!battery.rearms_overlay_notice());
        for (percentage, rearms) in [(20, false), (21, false), (25, false), (26, true)] {
            battery.percentage = Some(percentage);
            assert_eq!(battery.rearms_warning(), rearms);
        }
    }
}
