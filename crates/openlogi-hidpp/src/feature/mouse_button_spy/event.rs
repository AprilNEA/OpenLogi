use crate::feature::DecodeEvent;

use super::MouseButtonMask;

/// Event emitted by `0x8110` while the spy is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum MouseButtonSpyEvent {
    /// The current state of every button the spy tracks.
    Buttons(MouseButtonMask),
}

impl DecodeEvent for MouseButtonSpyEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        match sub_id {
            0 => Some(Self::Buttons(MouseButtonMask::from_bits(
                u16::from_be_bytes([payload[0], payload[1]]),
            ))),
            _ => None,
        }
    }
}
