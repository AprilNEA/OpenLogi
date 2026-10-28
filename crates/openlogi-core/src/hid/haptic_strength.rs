//! Strength of a device's haptic engine — pure data, no I/O.
//!
//! The HID++ write that carries a [`HapticStrength`] lives in
//! `openlogi_device::write::haptic`.

use az::SaturatingAs;
use nutype::nutype;

const RAW_MIN: u8 = 1;
const RAW_MAX: u8 = 100;

/// How strongly a device's haptic engine plays every waveform, in percent.
///
/// HID++ `0x19b0 HapticFeedback` takes `0..=100` for the whole device — the
/// Actions Ring, event haptics, and every other play alike. `0` silences the
/// engine, which is the user's "off" in Logi Options+ and stays theirs to
/// set, so this type holds `1..=100`. The firmware may round what it is
/// given: the MX Master 4 keeps 63 as 60.
#[nutype(
    const_fn,
    validate(greater_or_equal = RAW_MIN, less_or_equal = RAW_MAX),
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
        Serialize,
        Deserialize
    )
)]
pub struct HapticStrength(u8);

impl HapticStrength {
    /// The strength the MX Master 4 ships with.
    pub const DEFAULT: Self = match Self::try_new(60) {
        Ok(strength) => strength,
        Err(_) => panic!("valid default haptic strength"),
    };

    /// The faintest strength.
    pub const MIN: Self = match Self::try_new(RAW_MIN) {
        Ok(strength) => strength,
        Err(_) => panic!("valid minimum haptic strength"),
    };

    /// The strongest strength.
    pub const MAX: Self = match Self::try_new(RAW_MAX) {
        Ok(strength) => strength,
        Err(_) => panic!("valid maximum haptic strength"),
    };

    /// The strength nearest to `value`, a slider position in percent: one
    /// outside the range counts as the nearer end, and `NaN` as the faintest.
    #[must_use]
    pub fn from_rounded(value: f32) -> Self {
        let value = if value.is_nan() {
            f32::from(RAW_MIN)
        } else {
            value
        };
        let raw = value
            .clamp(f32::from(RAW_MIN), f32::from(RAW_MAX))
            .round()
            .saturating_as::<u8>();
        let Ok(strength) = Self::try_new(raw) else {
            unreachable!("a clamped haptic strength is always valid");
        };
        strength
    }
}

impl From<HapticStrength> for f32 {
    fn from(strength: HapticStrength) -> Self {
        Self::from(strength.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::HapticStrength;

    #[test]
    fn only_audible_percentages_parse() {
        // 0 is the user's off.
        HapticStrength::try_new(0).unwrap_err();
        assert_eq!(
            HapticStrength::try_new(100).map(HapticStrength::into_inner),
            Ok(100)
        );
        HapticStrength::try_new(101).unwrap_err();
    }

    #[test]
    fn a_slider_position_rounds_into_the_range() {
        let at = |value| HapticStrength::from_rounded(value).into_inner();
        assert_eq!(at(59.6), 60);
        assert_eq!(at(0.0), 1);
        assert_eq!(at(140.0), 100);
        assert_eq!(at(f32::NAN), 1);
    }
}
