//! Implements the `PointerMotionScaling` feature (ID `0x2205`), a
//! device-specific multiplier applied to pointer motion.
//!
//! The value is 8.8 fixed point: `0x0100` is 1×. The spec recommends
//! `0x0010`–`0x1000` and says firmware clips out-of-range writes, but an MX
//! Ergo (`MPM06.03_B0022`) was observed storing both `0x0001` and `0xffff`
//! unchanged — callers must bound the values they write themselves.

use std::num::NonZeroU16;

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// A pointer-motion multiplier in 8.8 fixed point (`0x0100` is 1×).
///
/// Zero is excluded: it is not a meaningful multiplier, so a zero on the wire
/// surfaces as [`Hidpp20Error::UnsupportedResponse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PointerScaling(NonZeroU16);

impl PointerScaling {
    /// Unscaled pointer motion (1×), the spec's default.
    pub const ONE: Self = match NonZeroU16::new(0x0100) {
        Some(raw) => Self(raw),
        None => panic!("0x0100 is non-zero"),
    };

    /// Wraps a raw 8.8 fixed-point value; `None` for zero.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Option<Self> {
        match NonZeroU16::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    /// The raw 8.8 fixed-point value.
    #[must_use]
    pub const fn raw(self) -> u16 {
        self.0.get()
    }

    /// The multiplier as a float (`1.0` for [`Self::ONE`]).
    #[must_use]
    pub fn multiplier(self) -> f32 {
        f32::from(self.raw()) / 256.0
    }

    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Self::from_raw(u16::from_be_bytes([payload[0], payload[1]]))
            .ok_or(Hidpp20Error::UnsupportedResponse)
    }
}

/// Implements the `PointerMotionScaling` / `0x2205` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x2205, version = 0)]
pub struct PointerMotionScalingFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl PointerMotionScalingFeature {
    /// Retrieves the current scaling (`GetPointerScalingValue`).
    pub async fn get_pointer_scaling(&self) -> Result<PointerScaling, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        PointerScaling::from_payload(&payload)
    }

    /// Requests a new scaling (`SetPointerScalingValue`) and returns the value
    /// the device echoes. The spec allows firmware to clip the request, so read
    /// the value back when the applied scaling matters.
    pub async fn set_pointer_scaling(
        &self,
        scaling: PointerScaling,
    ) -> Result<PointerScaling, Hidpp20Error> {
        let [msb, lsb] = scaling.raw().to_be_bytes();
        let payload = self
            .endpoint
            .call(1, [msb, lsb, 0x00])
            .await?
            .extend_payload();
        PointerScaling::from_payload(&payload)
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::PointerScaling;
    use crate::protocol::v20::Hidpp20Error;

    fn payload(msb: u8, lsb: u8) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[0] = msb;
        payload[1] = lsb;
        payload
    }

    #[test]
    fn parses_scaling_most_significant_byte_first() {
        let scaling = PointerScaling::from_payload(&payload(0x01, 0x80)).unwrap();

        assert_eq!(scaling.raw(), 0x0180);
        assert!((scaling.multiplier() - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn rejects_a_zero_scaling() {
        assert_matches!(
            PointerScaling::from_payload(&payload(0x00, 0x00)),
            Err(Hidpp20Error::UnsupportedResponse)
        );
        assert_eq!(PointerScaling::from_raw(0), None);
    }

    #[test]
    fn one_is_the_unscaled_default() {
        assert_eq!(PointerScaling::ONE.raw(), 0x0100);
        assert!((PointerScaling::ONE.multiplier() - 1.0).abs() < f32::EPSILON);
    }
}
