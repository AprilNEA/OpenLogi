//! Decodes the `MacroRecord` feature (ID `0x8030`): a gaming keyboard's MR
//! key.
//!
//! Logitech publishes no function table for it. The state event is
//! reverse-engineered (Solaar's rule engine, checked on a G913): function `0`
//! carries a non-zero first byte while MR is held. The keyboard reports it
//! whatever owns the key, so OpenLogi only listens and calls no function.

use crate::feature::DecodeEvent;

/// The `MacroRecord` feature ID.
pub const FEATURE_ID: u16 = 0x8030;

/// An event reported on the `MacroRecord` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum MacroRecordEvent {
    /// Whether MR is held now.
    Held(bool),
}

impl DecodeEvent for MacroRecordEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then_some(Self::Held(payload[0] != 0))
    }
}

impl MacroRecordEvent {
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
    fn decodes_press_and_release() {
        let mut payload = [0; 16];
        payload[0] = 1;
        assert_eq!(
            MacroRecordEvent::decode(0, &payload),
            Some(MacroRecordEvent::Held(true))
        );
        payload[0] = 0;
        assert_eq!(
            MacroRecordEvent::decode(0, &payload),
            Some(MacroRecordEvent::Held(false))
        );
        assert_eq!(MacroRecordEvent::decode(1, &payload), None);
    }
}
