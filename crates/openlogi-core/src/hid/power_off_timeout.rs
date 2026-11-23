//! Headset automatic power-off timeout — pure data, no I/O.
//!
//! The HID++ reads and writes that carry a [`PowerOffTimeout`] live in
//! `openlogi_device::write::power_off_timeout`.

use nutype::nutype;

const RAW_MAX: u8 = 90;

/// How many idle minutes a headset waits before it switches itself off: `0`
/// is never.
///
/// HID++ `0x1F20` carries the timeout in one byte. HeadsetControl offers
/// `0..=90` for the G933, so a value outside that range does not parse.
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
pub struct PowerOffTimeout(u8);

impl PowerOffTimeout {
    /// The headset never switches itself off.
    pub const NEVER: Self = match Self::try_new(0) {
        Ok(timeout) => timeout,
        Err(_) => panic!("valid never power-off timeout"),
    };

    /// The longest timeout.
    pub const MAX: Self = match Self::try_new(RAW_MAX) {
        Ok(timeout) => timeout,
        Err(_) => panic!("valid maximum power-off timeout"),
    };

    /// The timeout nearest to `raw` minutes inside the range: anything above
    /// 90 is [`Self::MAX`].
    #[must_use]
    pub fn clamped(raw: u8) -> Self {
        let Ok(timeout) = Self::try_new(raw.min(RAW_MAX)) else {
            unreachable!("a clamped power-off timeout is always valid");
        };
        timeout
    }

    /// The timeout in minutes, `0` meaning never.
    #[must_use]
    pub fn minutes(self) -> u8 {
        self.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::PowerOffTimeout;

    #[test]
    fn only_zero_to_ninety_minutes_parse() {
        assert_eq!(PowerOffTimeout::try_new(0), Ok(PowerOffTimeout::NEVER));
        assert_eq!(PowerOffTimeout::try_new(90), Ok(PowerOffTimeout::MAX));
        assert!(
            PowerOffTimeout::try_new(91).is_err(),
            "91 is above the range"
        );
    }

    #[test]
    fn clamping_keeps_values_in_range_and_caps_the_rest() {
        assert_eq!(PowerOffTimeout::clamped(15).minutes(), 15);
        assert_eq!(PowerOffTimeout::clamped(255), PowerOffTimeout::MAX);
    }

    #[derive(Debug, serde::Deserialize)]
    struct Holder {
        timeout: PowerOffTimeout,
    }

    #[test]
    fn deserializing_rejects_an_out_of_range_timeout() {
        assert!(
            toml::from_str::<Holder>("timeout = 91").is_err(),
            "the wire type keeps the range"
        );
        let holder: Holder = toml::from_str("timeout = 30").unwrap();
        assert_eq!(holder.timeout.minutes(), 30);
    }
}
