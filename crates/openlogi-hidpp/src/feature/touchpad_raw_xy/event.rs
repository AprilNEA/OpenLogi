//! The raw touch-data event emitted by `TouchpadRawXy` (`0x6100`).

/// One touch point from a [`DualXyData`] frame.
///
/// `x`/`y` are 14-bit device coordinates. The `z` and `area` bytes are
/// mode-dependent: in the default layout they are the Z distance and touch area,
/// but the active report flags (see
/// [`set_raw_report_state`](super::TouchpadRawXyFeature::set_raw_report_state))
/// can repurpose them — e.g. a 16-bit force spans both bytes, or `area` carries
/// width/height or major/minor data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TouchPoint {
    /// Contact type (2-bit): `0` = finger, others reserved.
    pub contact_type: u8,
    /// Contact status (2-bit): `0` = hover, `1` = touch, others reserved.
    pub contact_status: u8,
    /// 14-bit X coordinate of the touch centre.
    pub x: u16,
    /// 14-bit Y coordinate of the touch centre.
    pub y: u16,
    /// Unique finger ID (4-bit).
    pub finger_id: u8,
    /// Z distance, or force MSB in 16-bit-force mode (mode-dependent).
    pub z: u8,
    /// Touch area, or width/height / major-minor / force LSB (mode-dependent).
    pub area: u8,
}

/// A frame of raw touch data, carrying up to two touch points.
///
/// Frames describing more than two fingers are split across several events with
/// the same `timestamp`; the last sets `end_of_frame`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DualXyData {
    /// Running frame timestamp (unit from
    /// [`TouchpadInfo::timestamp_units`](super::TouchpadInfo::timestamp_units)).
    pub timestamp: u16,
    /// First touch point.
    pub touch1: TouchPoint,
    /// Second touch point.
    pub touch2: TouchPoint,
    /// Whether the physical switch under the surface is pressed.
    pub button: bool,
    /// Whether this is the last event for the frame.
    pub end_of_frame: bool,
    /// Total number of fingers in the frame.
    pub finger_count: u8,
}

/// An event emitted by [`TouchpadRawXyFeature`](super::TouchpadRawXyFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum TouchpadRawEvent {
    /// A new frame of raw touch data (up to two touch points).
    ///
    /// Only reported while raw reporting is enabled (see
    /// [`set_raw_report_state`](super::TouchpadRawXyFeature::set_raw_report_state)).
    DualXy(DualXyData),
}

/// Extracts a 14-bit coordinate from a high byte (low 6 bits) and a low byte.
fn coord14(high: u8, low: u8) -> u16 {
    (u16::from(high & 0x3f) << 8) | u16::from(low)
}

/// Decodes the `0x6100` event payload by its sub-id (default report layout).
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<TouchpadRawEvent> {
    match sub_id {
        0 => Some(TouchpadRawEvent::DualXy(DualXyData {
            timestamp: u16::from_be_bytes([payload[0], payload[1]]),
            touch1: TouchPoint {
                contact_type: payload[2] >> 6,
                contact_status: payload[4] >> 6,
                x: coord14(payload[2], payload[3]),
                y: coord14(payload[4], payload[5]),
                finger_id: payload[8] >> 4,
                z: payload[6],
                area: payload[7],
            },
            touch2: TouchPoint {
                contact_type: payload[9] >> 6,
                contact_status: payload[11] >> 6,
                x: coord14(payload[9], payload[10]),
                y: coord14(payload[11], payload[12]),
                finger_id: payload[15] >> 4,
                z: payload[13],
                area: payload[14],
            },
            // Byte 8 carries frame-level flags alongside touch 1's finger id.
            button: payload[8] & (1 << 2) != 0,
            end_of_frame: payload[8] & 1 != 0,
            finger_count: payload[15] & 0x0f,
        })),
        _ => None,
    }
}
