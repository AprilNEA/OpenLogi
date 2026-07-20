//! Events emitted by `ExtendedAdjustableDpi` (`0x2202`).

use super::types::{DpiDirection, Lod};

/// An event emitted by [`ExtendedDpiFeature`](super::ExtendedDpiFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ExtendedDpiEvent {
    /// The sensor's DPI parameters changed on the device (e.g. via a DPI
    /// button).
    ParametersChanged(DpiParametersChanged),
    /// A DPI calibration finished or timed out.
    CalibrationCompleted(DpiCalibrationCompleted),
}

/// Payload of [`ExtendedDpiEvent::ParametersChanged`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DpiParametersChanged {
    /// Index of the sensor whose parameters changed.
    pub sensor_index: u8,
    /// New X-axis DPI.
    pub dpi_x: u16,
    /// New Y-axis DPI, or `0` when the sensor has no independent Y axis.
    pub dpi_y: u16,
    /// New lift-off distance, or `None` when the device reported a value this
    /// crate does not model (the rest of the event is still delivered).
    pub lod: Option<Lod>,
}

/// Payload of [`ExtendedDpiEvent::CalibrationCompleted`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DpiCalibrationCompleted {
    /// Index of the sensor.
    pub sensor_index: u8,
    /// Axis that was calibrated.
    pub direction: DpiDirection,
    /// Calibration correction value; [`i16::MIN`] (`0x8000`) signals a
    /// sensor-level calibration failure (see [`Self::failed`]).
    pub correction: i16,
    /// Perpendicular-axis displacement, in pixel counts, that software can use to
    /// judge calibration quality.
    pub delta: i16,
}

impl DpiCalibrationCompleted {
    /// Whether the calibration failed at the sensor level (`correction` is the
    /// `0x8000` "negative zero" sentinel).
    #[must_use]
    pub fn failed(&self) -> bool {
        self.correction == i16::MIN
    }
}

/// Decodes an unsolicited `0x2202` event payload by its sub-id.
///
/// Returns `None` for sub-ids that do not correspond to a known event or whose
/// payload carries an unsupported enum value.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<ExtendedDpiEvent> {
    match sub_id {
        0 => Some(ExtendedDpiEvent::ParametersChanged(DpiParametersChanged {
            sensor_index: payload[0],
            dpi_x: u16::from_be_bytes([payload[1], payload[2]]),
            dpi_y: u16::from_be_bytes([payload[3], payload[4]]),
            lod: Lod::try_from(payload[5]).ok(),
        })),
        1 => Some(ExtendedDpiEvent::CalibrationCompleted(
            DpiCalibrationCompleted {
                sensor_index: payload[0],
                direction: DpiDirection::from(payload[1]),
                correction: i16::from_be_bytes([payload[2], payload[3]]),
                delta: i16::from_be_bytes([payload[4], payload[5]]),
            },
        )),
        _ => None,
    }
}
