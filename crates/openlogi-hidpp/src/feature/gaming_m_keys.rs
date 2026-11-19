//! Decodes the `GamingMKeys` feature (ID `0x8020`): the M1–M3 keys that pick
//! a gaming keyboard's onboard bank.
//!
//! Logitech publishes no function table for it. The state event is
//! reverse-engineered (Solaar's rule engine, checked on a G913): function `0`
//! carries the held M-keys as a bit mask in its first byte, bit `n` being
//! M`n+1`. The keyboard reports it whatever owns the keys, so OpenLogi only
//! listens and calls no function.

use crate::feature::DecodeEvent;

/// The `GamingMKeys` feature ID.
pub const FEATURE_ID: u16 = 0x8020;

bitflags::bitflags! {
    /// The M-keys a state event reports held. Unnamed bits are kept.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct MKeys: u8 {
        /// M1 is held.
        const M1 = 1 << 0;
        /// M2 is held.
        const M2 = 1 << 1;
        /// M3 is held.
        const M3 = 1 << 2;
    }
}

/// An event reported on the `GamingMKeys` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum GamingMKeysEvent {
    /// Every M-key held now.
    Held(MKeys),
}

impl DecodeEvent for GamingMKeysEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then(|| Self::Held(MKeys::from_bits_retain(payload[0])))
    }
}

impl GamingMKeysEvent {
    /// Decodes one unsolicited report on this feature's index.
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_held_mask() {
        let mut payload = [0; 16];
        payload[0] = (MKeys::M1 | MKeys::M3).bits();
        assert_eq!(
            GamingMKeysEvent::decode(0, &payload),
            Some(GamingMKeysEvent::Held(MKeys::M1 | MKeys::M3))
        );
        assert_eq!(GamingMKeysEvent::decode(1, &payload), None);
    }
}
