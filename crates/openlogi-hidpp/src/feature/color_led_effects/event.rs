//! Events emitted by the `ColorLedEffects` feature (`0x8070`).

use super::types::be16;

/// An event emitted by [`ColorLedEffectsFeature`](super::ColorLedEffectsFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ColorLedEffectsEvent {
    /// A period effect reached a synchronization point. Emitted once per period
    /// while sync events are enabled (see
    /// [`setSwControl`](super::ColorLedEffectsFeature::set_sw_control)).
    SyncEffect {
        /// Zone the event applies to; `0xff` means all zones.
        zone_index: u8,
        /// Current timing position within the period, in milliseconds.
        effect_counter: u16,
    },
}

/// Decodes an unsolicited `0x8070` event payload by its sub-id.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<ColorLedEffectsEvent> {
    match sub_id {
        0 => Some(ColorLedEffectsEvent::SyncEffect {
            zone_index: payload[0],
            effect_counter: be16(payload, 1),
        }),
        _ => None,
    }
}
