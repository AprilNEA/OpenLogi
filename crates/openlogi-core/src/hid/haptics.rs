//! Haptic feedback settings and read-back (HID++ `0x19b0`) — pure data, no
//! I/O.
//!
//! The HID++ reads and writes that produce and consume these types live in
//! `openlogi_device::write::haptic`.

use az::SaturatingAs;
use nutype::nutype;
use serde::{Deserialize, Serialize};

/// A haptic intensity percentage, `0..=100`, as the firmware stores it.
///
/// An MX Master 4 stores any value in the range exactly; a device whose
/// [`HapticLevels`] are restricted accepts only some of them, which the
/// write path checks before sending.
#[nutype(
    const_fn,
    validate(less_or_equal = 100),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        Hash,
        TryFrom,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct HapticIntensity(u8);

impl HapticIntensity {
    /// No feedback at all: the firmware plays nothing at this intensity.
    pub const OFF: Self = match Self::try_new(0) {
        Ok(intensity) => intensity,
        Err(_) => panic!("valid zero haptic intensity"),
    };

    /// The strongest feedback.
    pub const MAX: Self = match Self::try_new(100) {
        Ok(intensity) => intensity,
        Err(_) => panic!("valid maximum haptic intensity"),
    };

    /// Round and clamp a floating-point control value into the range.
    #[must_use]
    pub fn from_rounded(value: f32) -> Self {
        let value = if value.is_nan() { 0.0 } else { value };
        let raw = value.clamp(0.0, 100.0).round().saturating_as::<u8>();
        let Ok(value) = Self::try_new(raw) else {
            unreachable!("clamped haptic intensity is always valid");
        };
        value
    }
}

impl From<HapticIntensity> for f32 {
    fn from(intensity: HapticIntensity) -> Self {
        Self::from(intensity.into_inner())
    }
}

/// The device-wide haptic configuration the user sets: the firmware engine,
/// battery-saving haptics, and the intensity.
///
/// Persisted per device in `config.toml` and re-applied when the device
/// reconnects. Crosses the agent↔GUI IPC, so field order is wire format
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HapticSettings {
    /// The firmware haptic engine is on.
    pub enabled: bool,
    /// Battery-saving haptics are on.
    pub power_save: bool,
    /// How strong the feedback is.
    pub intensity: HapticIntensity,
}

impl HapticSettings {
    /// Whether these settings play no feedback at all: the engine is off, or
    /// it is on at zero intensity. Every consumer that silences feedback —
    /// the Actions Ring, the preview after a write, the settings app's
    /// controls — asks this rather than reading the fields.
    #[must_use]
    pub fn is_silent(self) -> bool {
        !self.enabled || self.intensity == HapticIntensity::OFF
    }
}

/// The intensity levels a device accepts.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant index, so
/// variant order is wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HapticLevels {
    /// Any percentage from 0 to 100.
    Any,
    /// Only 25, 50, 75, and 100.
    Quarters,
    /// A restricted set OpenLogi cannot list, kept as the raw byte. No
    /// intensity is written to such a device.
    Other(u8),
}

/// What a device reports about its haptic feedback: the settings it holds
/// now, which intensities it accepts, and its factory intensity.
///
/// Crosses the agent↔GUI IPC, so field order is wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HapticReading {
    /// The configuration the device holds.
    pub settings: HapticSettings,
    /// The intensities the device accepts.
    pub levels: HapticLevels,
    /// The device's factory intensity percentage, as reported. Shown only;
    /// not validated, as a value above 100 must not fail the read.
    pub default_intensity: u8,
}

#[cfg(test)]
mod tests {
    use super::{HapticIntensity, HapticSettings};

    fn settings(enabled: bool, intensity: u8) -> HapticSettings {
        HapticSettings {
            enabled,
            power_save: false,
            intensity: HapticIntensity::try_new(intensity).expect("valid"),
        }
    }

    #[test]
    fn silent_when_the_engine_is_off_or_the_intensity_is_zero() {
        assert!(settings(false, 60).is_silent());
        assert!(settings(true, 0).is_silent());
        assert!(settings(false, 0).is_silent());
        assert!(!settings(true, 1).is_silent());
    }

    #[test]
    fn rounding_stays_inside_the_range() {
        assert_eq!(HapticIntensity::from_rounded(-3.0), HapticIntensity::OFF);
        assert_eq!(
            HapticIntensity::from_rounded(f32::NAN),
            HapticIntensity::OFF
        );
        assert_eq!(HapticIntensity::from_rounded(250.0), HapticIntensity::MAX);
        assert_eq!(
            HapticIntensity::from_rounded(59.6),
            HapticIntensity::try_new(60).expect("valid")
        );
        HapticIntensity::try_new(101).expect_err("101 is above the range");
    }
}
