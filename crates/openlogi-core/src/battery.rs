//! Battery presentation and warning policies, shared by the UI, tray and overlay.

use crate::device::{BatteryFreshness, BatteryInfo, BatteryLevel, BatteryStatus};

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
    use crate::device::BatteryAttention;

    #[test]
    fn warning_policy_uses_current_percentage_not_firmware_bucket() {
        for (percentage, expected) in [
            (0, Some(BatteryAttention::Critical)),
            (10, Some(BatteryAttention::Critical)),
            (11, Some(BatteryAttention::Low)),
            (20, Some(BatteryAttention::Low)),
            (21, None),
            (100, None),
            (255, None),
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
                assert_eq!(battery.attention(), expected);
                for status in [
                    BatteryStatus::Charging,
                    BatteryStatus::ChargingSlow,
                    BatteryStatus::Full,
                ] {
                    battery.status = status;
                    assert_eq!(battery.attention(), None);
                }
                battery.status = BatteryStatus::Discharging;
                battery.freshness = BatteryFreshness::Cached;
                assert_eq!(battery.usable_percentage(), None);
                assert_eq!(battery.attention(), None);
                assert!(!battery.rearms_warning());
            }
        }
    }

    #[test]
    fn level_only_readings_keep_attention_without_inventing_percentages() {
        for (level, severity) in [
            (BatteryLevel::Critical, Some(BatteryAttention::Critical)),
            (BatteryLevel::Low, Some(BatteryAttention::Low)),
            (BatteryLevel::Good, None),
            (BatteryLevel::Unknown, None),
        ] {
            let mut battery = BatteryInfo {
                percentage: None,
                level,
                status: BatteryStatus::Discharging,
                freshness: BatteryFreshness::Current,
            };
            assert_eq!(battery.attention(), severity);
            assert_eq!(battery.usable_percentage(), None);
            assert!(!battery.rearms_warning());
            battery.freshness = BatteryFreshness::Cached;
            assert_eq!(battery.attention(), None);
            battery.freshness = BatteryFreshness::Current;
            battery.status = BatteryStatus::Charging;
            assert_eq!(battery.attention(), None);
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
