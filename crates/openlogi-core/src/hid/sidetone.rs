//! Headset sidetone level — pure data, no I/O.
//!
//! The HID++ reads and writes that carry a [`SidetoneLevel`] live in
//! `openlogi_device::write::sidetone`.

use nutype::nutype;

const RAW_MAX: u8 = 100;

/// How much of the microphone a headset plays back into its own earcups, in
/// percent: `0` is off.
///
/// HID++ `0x8300 Sidetone` documents `0..=100` and the device rejects
/// anything above, so a value outside that range does not parse.
#[nutype(
    const_fn,
    validate(less_or_equal = RAW_MAX),
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
pub struct SidetoneLevel(u8);

impl SidetoneLevel {
    /// Sidetone off.
    pub const OFF: Self = match Self::try_new(0) {
        Ok(level) => level,
        Err(_) => panic!("valid minimum sidetone level"),
    };

    /// The loudest sidetone.
    pub const MAX: Self = match Self::try_new(RAW_MAX) {
        Ok(level) => level,
        Err(_) => panic!("valid maximum sidetone level"),
    };

    /// The level nearest to `raw` inside the range: anything above 100 is
    /// [`Self::MAX`].
    #[must_use]
    pub fn clamped(raw: u8) -> Self {
        let Ok(level) = Self::try_new(raw.min(RAW_MAX)) else {
            unreachable!("a clamped sidetone level is always valid");
        };
        level
    }

    /// The level as a percent.
    #[must_use]
    pub fn percent(self) -> u8 {
        self.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::SidetoneLevel;

    #[test]
    fn only_zero_to_one_hundred_parse() {
        assert_eq!(SidetoneLevel::try_new(0), Ok(SidetoneLevel::OFF));
        assert_eq!(SidetoneLevel::try_new(100), Ok(SidetoneLevel::MAX));
        assert!(
            SidetoneLevel::try_new(101).is_err(),
            "101 is above the range"
        );
    }

    #[derive(Debug, serde::Deserialize)]
    struct Holder {
        level: SidetoneLevel,
    }

    #[test]
    fn deserializing_rejects_an_out_of_range_level() {
        assert!(
            toml::from_str::<Holder>("level = 101").is_err(),
            "the wire type keeps the range"
        );
        let holder: Holder = toml::from_str("level = 40").unwrap();
        assert_eq!(holder.level.percent(), 40);
    }
}
