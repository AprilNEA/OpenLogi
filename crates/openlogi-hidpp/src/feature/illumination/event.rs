//! Events emitted by the `Illumination` feature (`0x1990`).

use super::types::{BrightnessClampedSource, IlluminationState, be16, illumination_state};

/// An event emitted by [`IlluminationFeature`](super::IlluminationFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum IlluminationEvent {
    /// The on/off illumination state changed.
    IlluminationChanged(IlluminationState),
    /// The brightness changed, in Lumens.
    BrightnessChanged(u16),
    /// The color temperature changed, in Kelvin.
    ColorTemperatureChanged(u16),
    /// The effective maximum brightness changed, in Lumens (`0` = no effective
    /// maximum). Requires feature version 1.
    BrightnessEffectiveMaxChanged(u16),
    /// A brightness request was clamped to the effective maximum. Requires
    /// feature version 1.
    BrightnessClamped {
        /// What triggered the clamp.
        source: BrightnessClampedSource,
        /// The clamped brightness, equal to the current effective maximum, in
        /// Lumens.
        brightness: u16,
    },
}

/// Decodes an unsolicited `0x1990` event payload by its sub-id.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<IlluminationEvent> {
    match sub_id {
        0 => Some(IlluminationEvent::IlluminationChanged(
            illumination_state(payload[0]).ok()?,
        )),
        1 => Some(IlluminationEvent::BrightnessChanged(be16(payload, 0))),
        2 => Some(IlluminationEvent::ColorTemperatureChanged(be16(payload, 0))),
        3 => Some(IlluminationEvent::BrightnessEffectiveMaxChanged(be16(
            payload, 0,
        ))),
        4 => Some(IlluminationEvent::BrightnessClamped {
            source: BrightnessClampedSource::try_from(payload[0]).ok()?,
            brightness: be16(payload, 1),
        }),
        _ => None,
    }
}
