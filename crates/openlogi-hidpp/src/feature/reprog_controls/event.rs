use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{nibble::U4, protocol::v20};

use super::ControlId;

fn i16_from_be_payload(bytes: &[u8]) -> i16 {
    i16::from_be_bytes(bytes.try_into().unwrap())
}

/// One analytics key event entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AnalyticsKeyEvent {
    /// Control ID associated with the analytics event.
    pub cid: ControlId,
    /// Device-defined analytics event code.
    pub event: u8,
}

impl AnalyticsKeyEvent {
    fn from_payload(bytes: &[u8]) -> Self {
        Self {
            cid: ControlId::from_payload(&bytes[0..=1]),
            event: bytes[2],
        }
    }
}

/// Raw wheel movement resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u8)]
pub enum RawWheelResolution {
    /// Low-resolution wheel movement.
    Low = 0,
    /// High-resolution wheel movement.
    High = 1,
}

/// Event emitted by `0x1b04`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ReprogControlsEvent {
    /// Up to four currently pressed diverted controls.
    DivertedButtons([ControlId; 4]),
    /// Raw pointer movement reported while a diverted control is held.
    DivertedRawMouseXy {
        /// Horizontal delta.
        dx: i16,
        /// Vertical delta.
        dy: i16,
    },
    /// Batch of analytics key event entries.
    AnalyticsKeyEvents([AnalyticsKeyEvent; 5]),
    /// Raw wheel movement reported while a diverted control is held.
    DivertedRawWheel {
        /// Wheel movement resolution.
        resolution: RawWheelResolution,
        /// Number of wheel periods encoded in the event header.
        periods: U4,
        /// Vertical wheel delta.
        delta_vertical: i16,
    },
}

impl ReprogControlsEvent {
    /// Whether `cid` is currently reported as pressed in a diverted-buttons event.
    #[must_use]
    pub fn is_pressed(self, cid: ControlId) -> bool {
        matches!(self, Self::DivertedButtons(cids) if cids.contains(&cid))
    }
}

/// Decode an unsolicited `0x1b04` HID++ message.
#[must_use]
pub fn decode_event(
    msg: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<ReprogControlsEvent> {
    let header = msg.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.software_id.to_lo() != 0
    {
        return None;
    }
    decode_event_payload(header.function_id.to_lo(), &msg.extend_payload())
}

pub(super) fn decode_event_payload(
    function_id: u8,
    payload: &[u8; 16],
) -> Option<ReprogControlsEvent> {
    match function_id {
        0 => Some(ReprogControlsEvent::DivertedButtons([
            ControlId::from_payload(&payload[0..=1]),
            ControlId::from_payload(&payload[2..=3]),
            ControlId::from_payload(&payload[4..=5]),
            ControlId::from_payload(&payload[6..=7]),
        ])),
        1 => Some(ReprogControlsEvent::DivertedRawMouseXy {
            dx: i16_from_be_payload(&payload[0..=1]),
            dy: i16_from_be_payload(&payload[2..=3]),
        }),
        2 => Some(ReprogControlsEvent::AnalyticsKeyEvents([
            AnalyticsKeyEvent::from_payload(&payload[0..=2]),
            AnalyticsKeyEvent::from_payload(&payload[3..=5]),
            AnalyticsKeyEvent::from_payload(&payload[6..=8]),
            AnalyticsKeyEvent::from_payload(&payload[9..=11]),
            AnalyticsKeyEvent::from_payload(&payload[12..=14]),
        ])),
        4 => Some(ReprogControlsEvent::DivertedRawWheel {
            resolution: RawWheelResolution::try_from((payload[0] >> 4) & 1).ok()?,
            periods: U4::from_lo(payload[0]),
            delta_vertical: i16_from_be_payload(&payload[1..=2]),
        }),
        _ => None,
    }
}
