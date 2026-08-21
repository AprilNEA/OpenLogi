//! Capability types shared by standalone light drivers and their clients.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The native unit used by a standalone light control range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightValueUnit {
    /// A percentage of the device's supported range.
    Percent,
    /// Absolute luminous output, where the protocol exposes lumens.
    Lumens,
    /// Colour temperature in Kelvin.
    Kelvin,
}

/// A validated light value range advertised by a device driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct LightValueRange {
    /// Inclusive lower bound in [`Self::unit`].
    min: u16,
    /// Inclusive upper bound in [`Self::unit`].
    max: u16,
    /// Supported increment. Must be non-zero.
    step: u16,
    /// The unit represented by `min`, `max`, and `step`.
    unit: LightValueUnit,
}

impl LightValueRange {
    /// Construct a range after validating its bounds and quantization grid.
    ///
    /// The upper bound must lie on the same grid as the lower bound. This
    /// keeps driver quantization total: a valid range can never produce a
    /// value outside the advertised interval or between device-supported
    /// stops.
    pub const fn new(
        min: u16,
        max: u16,
        step: u16,
        unit: LightValueUnit,
    ) -> Result<Self, LightValueRangeError> {
        if min > max {
            return Err(LightValueRangeError::Reversed { min, max });
        }
        if step == 0 {
            return Err(LightValueRangeError::ZeroStep);
        }
        if !(max - min).is_multiple_of(step) {
            return Err(LightValueRangeError::Unaligned { min, max, step });
        }
        if matches!(unit, LightValueUnit::Percent) && max > 100 {
            return Err(LightValueRangeError::PercentOutOfBounds { min, max });
        }
        Ok(Self {
            min,
            max,
            step,
            unit,
        })
    }

    /// Inclusive lower bound in the advertised unit.
    #[must_use]
    pub const fn min(self) -> u16 {
        self.min
    }

    /// Inclusive upper bound in the advertised unit.
    #[must_use]
    pub const fn max(self) -> u16 {
        self.max
    }

    /// Supported increment.
    #[must_use]
    pub const fn step(self) -> u16 {
        self.step
    }

    /// Unit represented by this range.
    #[must_use]
    pub const fn unit(self) -> LightValueUnit {
        self.unit
    }

    /// Whether `value` is representable without clamping or quantization.
    #[must_use]
    pub fn contains(self, value: u16) -> bool {
        value >= self.min
            && value <= self.max
            && self.step != 0
            && (value - self.min).is_multiple_of(self.step)
    }

    /// Snap `value` to the nearest supported point inside this range.
    #[must_use]
    pub fn quantize(self, value: u16) -> u16 {
        let clamped = value.clamp(self.min, self.max);
        let offset = clamped - self.min;
        let lower = offset / self.step;
        let remainder = offset % self.step;
        let index = if remainder.saturating_mul(2) >= self.step {
            lower.saturating_add(1)
        } else {
            lower
        };
        self.min
            .saturating_add(index.saturating_mul(self.step))
            .min(self.max)
    }

    /// Map normalized brightness to the nearest native value in this range.
    #[must_use]
    pub fn native_for_percent(self, percent: u8) -> Option<u16> {
        if percent > 100 {
            return None;
        }
        let span = u32::from(self.max) - u32::from(self.min);
        let raw = u32::from(self.min) + (span * u32::from(percent) + 50) / 100;
        u16::try_from(raw).ok().map(|value| self.quantize(value))
    }

    /// Convert a supported native value to normalized brightness.
    #[must_use]
    pub fn percent_for_native(self, value: u16) -> Option<u8> {
        if !self.contains(value) {
            return None;
        }
        let span = u32::from(self.max) - u32::from(self.min);
        if span == 0 {
            return Some(0);
        }
        u8::try_from(((u32::from(value) - u32::from(self.min)) * 100 + span / 2) / span).ok()
    }
}

/// Validation failure for [`LightValueRange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LightValueRangeError {
    /// The lower bound is greater than the upper bound.
    #[error("light range minimum {min} is greater than maximum {max}")]
    Reversed {
        /// Rejected lower bound.
        min: u16,
        /// Rejected upper bound.
        max: u16,
    },
    /// A range cannot have a zero increment.
    #[error("light range step must be non-zero")]
    ZeroStep,
    /// The upper bound is not reachable from the lower bound using `step`.
    #[error("light range {min}..={max} is not aligned to step {step}")]
    Unaligned {
        /// Lower bound of the invalid range.
        min: u16,
        /// Upper bound of the invalid range.
        max: u16,
        /// Increment that does not reach the upper bound.
        step: u16,
    },
    /// Percentage ranges must stay within 0–100.
    #[error("percentage light range {min}..={max} exceeds 0..=100")]
    PercentOutOfBounds {
        /// Rejected lower bound.
        min: u16,
        /// Rejected upper bound.
        max: u16,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLightValueRange {
    min: u16,
    max: u16,
    step: u16,
    unit: LightValueUnit,
}

impl<'de> Deserialize<'de> for LightValueRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawLightValueRange::deserialize(deserializer)?;
        Self::new(raw.min, raw.max, raw.step, raw.unit).map_err(serde::de::Error::custom)
    }
}

/// Controls a standalone light driver can implement.
///
/// Optional ranges are the source of truth for UI controls. A driver must not
/// advertise a control merely because the product is classified as a light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightCapabilities {
    /// Whether the driver can switch the light on and off.
    pub power: bool,
    /// Supported brightness range, if brightness is controllable.
    pub brightness: Option<LightValueRange>,
    /// Supported colour-temperature range, if temperature is controllable.
    pub temperature: Option<LightValueRange>,
    /// Whether the driver can set a colour.
    #[serde(default)]
    pub color: bool,
    /// Whether the driver exposes independently-addressable zones.
    #[serde(default)]
    pub zones: bool,
}
