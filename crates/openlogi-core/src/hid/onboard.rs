//! Gaming-mouse onboard memory. Wire types: field order matters.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};

/// A report interval of 1–8 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ReportRate(u8);

impl ReportRate {
    /// Longest interval.
    pub const MAX_MS: u8 = 8;

    /// The rate for `ms` milliseconds.
    #[must_use]
    pub const fn from_ms(ms: u8) -> Option<Self> {
        if ms >= 1 && ms <= Self::MAX_MS {
            Some(Self(ms))
        } else {
            None
        }
    }

    /// Interval in milliseconds.
    #[must_use]
    pub const fn ms(self) -> u8 {
        self.0
    }

    /// Reports per second.
    #[must_use]
    pub const fn hz(self) -> u16 {
        1000 / self.0 as u16
    }
}

impl fmt::Display for ReportRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.hz())
    }
}

impl<'de> Deserialize<'de> for ReportRate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let ms = u8::deserialize(deserializer)?;
        Self::from_ms(ms).ok_or_else(|| {
            de::Error::custom(format_args!(
                "report rate must be 1–{} ms, got {ms}",
                Self::MAX_MS
            ))
        })
    }
}

/// Current and supported report rates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRateInfo {
    /// Active rate.
    pub current: ReportRate,
    /// Supported rates, fastest first.
    pub supported: Vec<ReportRate>,
}

/// Who owns a gaming mouse's buttons, DPI levels and report rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OnboardMode {
    /// A stored profile.
    Onboard,
    /// OpenLogi.
    Host,
}

/// One onboard profile slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardProfile {
    /// 1-based slot number.
    pub index: u8,
    /// Stored name.
    pub name: Option<String>,
    /// Whether it can be made active.
    pub enabled: bool,
}

/// Onboard memory as read from the mouse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardState {
    /// Current owner.
    pub mode: OnboardMode,
    /// Active profile; `None` in host mode.
    pub active_profile: Option<u8>,
    /// Profile slots.
    pub profiles: Vec<OnboardProfile>,
    /// Report rate, if supported.
    pub report_rate: Option<ReportRateInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_rate_is_bounded_and_formats_as_hz() {
        assert_eq!(ReportRate::from_ms(0), None);
        assert_eq!(ReportRate::from_ms(9), None);
        let rate = ReportRate::from_ms(1).unwrap();
        assert_eq!(rate.to_string(), "1000 Hz");
        assert_eq!(ReportRate::from_ms(8).unwrap().hz(), 125);
    }

    #[test]
    fn report_rate_rejects_out_of_range_config() {
        #[derive(Debug, Deserialize)]
        struct Wrapper {
            v: ReportRate,
        }
        assert_eq!(toml::from_str::<Wrapper>("v = 2").unwrap().v.ms(), 2);
        toml::from_str::<Wrapper>("v = 12").unwrap_err();
    }
}
