//! Events emitted by `TouchMouseRaw` (`0x6110`).

/// Number of touch points carried by a raw-data report.
pub const TOUCH_POINT_COUNT: usize = 4;
/// Byte value in a coordinate's high byte that marks a lifted (absent) finger.
const LIFTED: u8 = 0xff;

bitflags::bitflags! {
    /// Mouse status flags from [`TouchMouseRawEvent::StatusChanged`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct TouchMouseStatus: u8 {
        /// The mouse is lifted off the surface.
        const MOUSE_LIFTED = 1 << 0;
        /// A mouse button is pressed.
        const BUTTON_DOWN = 1 << 1;
    }
}

/// A single touch point in a raw-data report.
///
/// The finger ID is the touch point's position (`0..4`) in the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TouchMousePoint {
    /// 12-bit X coordinate.
    pub x: u16,
    /// 12-bit Y coordinate.
    pub y: u16,
    /// Contact width along X (4-bit), or Z in the Z-reporting mode.
    pub width_x: u8,
    /// Contact width along Y (4-bit).
    pub width_y: u8,
}

/// An event emitted by [`TouchMouseRawFeature`](super::TouchMouseRawFeature).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum TouchMouseRawEvent {
    /// Raw data for up to four touch points; a lifted finger is `None`.
    ///
    /// Only reported in a raw mode (see
    /// [`set_raw_mode`](super::TouchMouseRawFeature::set_raw_mode)).
    RawData {
        /// Touch points indexed by finger ID (`0..4`).
        touches: [Option<TouchMousePoint>; TOUCH_POINT_COUNT],
    },
    /// A mouse status flag changed.
    StatusChanged(TouchMouseStatus),
}

/// Decodes one touch point's four bytes, or `None` when the finger is lifted.
fn decode_touch(bytes: &[u8]) -> Option<TouchMousePoint> {
    let [x_high, y_high, low_nibbles, widths] = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if x_high == LIFTED {
        return None;
    }
    Some(TouchMousePoint {
        // X takes the high 8 bits from `x_high` and the low 4 from the low nibble.
        x: (u16::from(x_high) << 4) | u16::from(low_nibbles & 0x0f),
        // Y takes the high 8 bits from `y_high` and the low 4 from the high nibble.
        y: (u16::from(y_high) << 4) | u16::from(low_nibbles >> 4),
        width_x: widths & 0x0f,
        width_y: widths >> 4,
    })
}

/// Decodes the `0x6110` event payload by its sub-id.
pub(super) fn decode_event(sub_id: u8, payload: &[u8; 16]) -> Option<TouchMouseRawEvent> {
    match sub_id {
        0 => {
            let mut touches = [None; TOUCH_POINT_COUNT];
            for (i, touch) in touches.iter_mut().enumerate() {
                *touch = decode_touch(&payload[i * 4..i * 4 + 4]);
            }
            Some(TouchMouseRawEvent::RawData { touches })
        }
        1 => Some(TouchMouseRawEvent::StatusChanged(
            TouchMouseStatus::from_bits_retain(payload[0]),
        )),
        _ => None,
    }
}
