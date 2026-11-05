//! Implements the `GamingGKeys` feature (ID `0x8010`): a gaming keyboard's
//! dedicated G-key column.
//!
//! Logitech's public feature index names this feature but publishes no
//! function table. Everything here is reverse-engineered, after Solaar's
//! `divert-gkeys` setting and rule engine, and must be read as such:
//!
//! - Function `0` answers the number of G-keys in its first byte (`5` on a
//!   G913).
//! - Function `2` with `1` hands the whole row to software: the keys stop
//!   running their onboard actions and report a state event instead. `0`
//!   hands the row back. There is no per-key ownership, and the keyboard
//!   forgets the setting when it power-cycles.
//! - The state event (function `0`) carries the held keys as a
//!   little-endian bit mask in its first four bytes, bit `n` being G`n+1`.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

const FN_GET_COUNT: u8 = 0;
const FN_SET_SOFTWARE_CONTROL: u8 = 2;

/// Implements the `GamingGKeys` / `0x8010` feature.
#[derive(Feature)]
#[creatable(id = 0x8010, version = 0)]
pub struct GamingGKeysFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

bitflags::bitflags! {
    /// The G-keys a state event reports held. Bit `n` is G`n+1`; bits past
    /// the named keys are kept, so a longer row still decodes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct GKeys: u32 {
        /// G1 is held.
        const G1 = 1 << 0;
        /// G2 is held.
        const G2 = 1 << 1;
        /// G3 is held.
        const G3 = 1 << 2;
        /// G4 is held.
        const G4 = 1 << 3;
        /// G5 is held.
        const G5 = 1 << 4;
    }
}

/// An event emitted by [`GamingGKeysFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum GamingGKeysEvent {
    /// Every G-key held now.
    Held(GKeys),
}

impl DecodeEvent for GamingGKeysEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then(|| {
            Self::Held(GKeys::from_bits_retain(u32::from_le_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ])))
        })
    }
}

impl GamingGKeysEvent {
    /// Decodes one unsolicited report on this feature's index.
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
    }
}

impl GamingGKeysFeature {
    /// The number of G-keys in the row.
    pub async fn key_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self
            .endpoint
            .call(FN_GET_COUNT, [0; 3])
            .await?
            .extend_payload()[0])
    }

    /// Hand the whole G-key row to software, which then receives
    /// [`GamingGKeysEvent`]s instead of the keys' onboard actions.
    ///
    /// Both ownership writes finish on the wire before they return, so a
    /// release can never overtake a stalled take and leave the row captured
    /// with nobody listening. The caller must drive each to completion.
    pub async fn take_row(&self) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call_write_through(FN_SET_SOFTWARE_CONTROL, [1, 0, 0])
            .await?;
        Ok(())
    }

    /// Hand the G-key row back to its onboard actions.
    pub async fn release_row(&self) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call_write_through(FN_SET_SOFTWARE_CONTROL, [0, 0, 0])
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_little_endian_mask() {
        let mut payload = [0; 16];
        payload[0] = (GKeys::G1 | GKeys::G5).bits().to_le_bytes()[0];
        payload[1] = 0x01;
        assert_eq!(
            GamingGKeysEvent::decode(0, &payload),
            Some(GamingGKeysEvent::Held(
                GKeys::G1 | GKeys::G5 | GKeys::from_bits_retain(1 << 8)
            ))
        );
        assert_eq!(GamingGKeysEvent::decode(1, &payload), None);
    }
}
