//! Pointer speed of a mouse that scales its own motion — pure data, no I/O.
//!
//! The HID++ reads and writes that carry a [`PointerSpeed`] live in
//! `openlogi_device::write::pointer_scaling`.

use az::SaturatingAs;
use nutype::nutype;

const RAW_MIN: u16 = 0x0020;
const RAW_DEFAULT: u16 = 0x0100;
const RAW_MAX: u16 = 0x0300;

/// The factor a mouse multiplies its sensor motion by, in 1/256 steps: `256`
/// is 1×, the speed the firmware starts at.
///
/// HID++ `0x2205 PointerMotionScaling` takes this factor on mice that have no
/// adjustable DPI, such as the M590 and the MX Ergo. The firmware stores any
/// 16-bit value without clipping it, and a value near `0` stops the pointer
/// (#1372). This type therefore holds only the range Logi Options+ writes,
/// `32..=768` (0.125× to 3×), and a value outside it does not parse.
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
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct PointerSpeed(u16);

impl PointerSpeed {
    /// The slowest speed, 0.125×. Precision mode moves the pointer at this
    /// speed, as in Logi Options+.
    pub const MIN: Self = match Self::try_new(RAW_MIN) {
        Ok(speed) => speed,
        Err(_) => panic!("valid minimum pointer speed"),
    };

    /// 1×, the speed the firmware starts at.
    pub const DEFAULT: Self = match Self::try_new(RAW_DEFAULT) {
        Ok(speed) => speed,
        Err(_) => panic!("valid default pointer speed"),
    };

    /// The fastest speed, 3×.
    pub const MAX: Self = match Self::try_new(RAW_MAX) {
        Ok(speed) => speed,
        Err(_) => panic!("valid maximum pointer speed"),
    };

    /// The speed nearest to `raw` inside the range: a device reading of
    /// `0x0001` or `0xffff` (#1372) becomes [`Self::MIN`] or [`Self::MAX`].
    #[must_use]
    pub fn clamped(raw: u16) -> Self {
        let Ok(speed) = Self::try_new(raw.clamp(RAW_MIN, RAW_MAX)) else {
            unreachable!("a clamped pointer speed is always valid");
        };
        speed
    }

    /// The factor as a multiple of 1×.
    #[must_use]
    pub fn multiplier(self) -> f32 {
        f32::from(self.into_inner()) / f32::from(RAW_DEFAULT)
    }

    /// The speed at `position` on a `0..=100` slider. The slider is
    /// geometric on each side of its midpoint, as in Logi Options+: `0` is
    /// [`Self::MIN`], `50` is [`Self::DEFAULT`], `100` is [`Self::MAX`], and
    /// equal steps change the speed by equal ratios. A position outside
    /// `0..=100` counts as the nearer end, and `NaN` as the midpoint.
    #[must_use]
    pub fn from_slider(position: f32) -> Self {
        let position = if position.is_nan() {
            SLIDER_MID
        } else {
            position.clamp(0.0, SLIDER_MAX)
        };
        let offset = (position - SLIDER_MID) / SLIDER_MID;
        let end = if offset >= 0.0 { Self::MAX } else { Self::MIN };
        let multiplier = end.multiplier().powf(offset.abs());
        Self::clamped(
            (multiplier * f32::from(RAW_DEFAULT))
                .round()
                .saturating_as::<u16>(),
        )
    }

    /// Where this speed sits on the `0..=100` slider of
    /// [`Self::from_slider`].
    #[must_use]
    pub fn slider_position(self) -> f32 {
        let multiplier = self.multiplier();
        // Both ends lie a positive distance away in `ln` space: `ln 3` above
        // 1×, `-ln 0.125` below it.
        let offset = if multiplier >= 1.0 {
            multiplier.ln() / Self::MAX.multiplier().ln()
        } else {
            -(multiplier.ln() / Self::MIN.multiplier().ln())
        };
        SLIDER_MID + offset * SLIDER_MID
    }

    /// [`Self::slider_position`] rounded to a whole percent, the way Logi
    /// Options+ labels its slider.
    #[must_use]
    pub fn slider_percent(self) -> u8 {
        self.slider_position()
            .round()
            .clamp(0.0, SLIDER_MAX)
            .saturating_as::<u8>()
    }
}

/// The top of the slider [`PointerSpeed::from_slider`] maps.
pub const SLIDER_MAX: f32 = 100.0;
const SLIDER_MID: f32 = SLIDER_MAX / 2.0;

impl Default for PointerSpeed {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::{PointerSpeed, PointerSpeedError};

    #[test]
    fn holds_only_the_range_options_plus_writes() {
        assert!(matches!(
            PointerSpeed::try_new(0x001f),
            Err(PointerSpeedError::GreaterOrEqualViolated)
        ));
        assert!(matches!(
            PointerSpeed::try_new(0x0301),
            Err(PointerSpeedError::LessOrEqualViolated)
        ));
        assert_eq!(PointerSpeed::MIN.into_inner(), 0x0020);
        assert_eq!(PointerSpeed::DEFAULT.into_inner(), 0x0100);
        assert_eq!(PointerSpeed::MAX.into_inner(), 0x0300);
    }

    #[test]
    fn an_unclipped_device_reading_clamps_into_the_range() {
        assert_eq!(PointerSpeed::clamped(0x0001), PointerSpeed::MIN);
        assert_eq!(PointerSpeed::clamped(0xffff), PointerSpeed::MAX);
        assert_eq!(PointerSpeed::clamped(0x0180).into_inner(), 0x0180);
    }

    #[test]
    fn the_slider_is_geometric_around_one_times() {
        assert_eq!(PointerSpeed::from_slider(0.0), PointerSpeed::MIN);
        assert_eq!(PointerSpeed::from_slider(50.0), PointerSpeed::DEFAULT);
        assert_eq!(PointerSpeed::from_slider(100.0), PointerSpeed::MAX);
        // Halfway down is the geometric mean of 1× and 0.125×.
        assert_eq!(PointerSpeed::from_slider(25.0).into_inner(), 91);
        assert_eq!(PointerSpeed::from_slider(-10.0), PointerSpeed::MIN);
        assert_eq!(PointerSpeed::from_slider(250.0), PointerSpeed::MAX);
    }

    #[test]
    fn a_speed_returns_to_its_slider_position() {
        for raw in [0x0020, 0x005b, 0x0100, 0x01bb, 0x0300] {
            let speed = PointerSpeed::clamped(raw);
            assert_eq!(PointerSpeed::from_slider(speed.slider_position()), speed);
        }
        assert!((PointerSpeed::DEFAULT.slider_position() - 50.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_slider_label_is_a_whole_percent() {
        assert_eq!(PointerSpeed::MIN.slider_percent(), 0);
        assert_eq!(PointerSpeed::DEFAULT.slider_percent(), 50);
        assert_eq!(PointerSpeed::MAX.slider_percent(), 100);
        assert_eq!(PointerSpeed::from_slider(25.0).slider_percent(), 25);
    }

    #[test]
    fn serializes_as_the_raw_factor() -> Result<(), toml::de::Error> {
        #[derive(serde::Deserialize)]
        struct Holder {
            speed: PointerSpeed,
        }
        let holder: Holder = toml::from_str("speed = 384")?;
        assert_eq!(holder.speed.into_inner(), 384);
        assert!(toml::from_str::<Holder>("speed = 1").is_err());
        Ok(())
    }
}
