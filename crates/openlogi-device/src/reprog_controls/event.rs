use hidpp::protocol::v20;

use super::{ControlId, RawWheelResolution, ReprogControlsEvent, decode_full_event};

/// An unsolicited `0x1b04` event decoded for OpenLogi's gesture pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawControlEvent {
    /// `divertedButtonsEvent`: the (up to four) CIDs currently held down. A
    /// slot is `0` when fewer than four are pressed; an all-zero array means
    /// every diverted control was released.
    DivertedButtons([u16; 4]),
    /// `rawXYEvent`: signed movement deltas reported while a raw-XY control is
    /// held.
    RawXy {
        /// Horizontal delta (`+` = right, in the device's raw units).
        dx: i16,
        /// Vertical delta (`+` = down, in the device's raw units).
        dy: i16,
    },
}

impl RawControlEvent {
    /// Whether `cid` is among the controls reported as currently pressed.
    #[must_use]
    pub fn is_pressed(&self, cid: u16) -> bool {
        matches!(self, Self::DivertedButtons(cids) if cids.contains(&cid))
    }
}

impl TryFrom<ReprogControlsEvent> for RawControlEvent {
    type Error = ReprogControlsEvent;

    fn try_from(event: ReprogControlsEvent) -> Result<Self, Self::Error> {
        match event {
            ReprogControlsEvent::DivertedButtons(cids) => {
                Ok(Self::DivertedButtons(cids.map(ControlId::into)))
            }
            ReprogControlsEvent::DivertedRawMouseXy { dx, dy } => Ok(Self::RawXy { dx, dy }),
            ReprogControlsEvent::AnalyticsKeyEvents(_)
            | ReprogControlsEvent::DivertedRawWheel { .. } => Err(event),
        }
    }
}

/// Decode a channel message into a [`RawControlEvent`] when it is an unsolicited
/// `0x1b04` event for `(device_index, feature_index)`.
///
/// Returns `None` for request responses (`software_id != 0`), messages from a
/// different device or feature, and events outside OpenLogi's gesture-control
/// pipeline.
#[must_use]
pub fn decode_event(
    msg: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<RawControlEvent> {
    decode_full_event(msg, device_index, feature_index)?
        .try_into()
        .ok()
}

/// `divertedRawWheel`: the main wheel's movement while a control diverted
/// with raw-wheel reporting is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawWheelEvent {
    /// Whether `delta` counts notches or high-resolution units.
    pub resolution: RawWheelResolution,
    /// Wheel movement; positive is away from the user, as in a HID wheel
    /// report.
    pub delta: i16,
}

/// Decode a channel message into a [`RawWheelEvent`] when it is an
/// unsolicited `divertedRawWheel` event for `(device_index, feature_index)`.
#[must_use]
pub fn decode_raw_wheel_event(
    msg: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<RawWheelEvent> {
    match decode_full_event(msg, device_index, feature_index)? {
        ReprogControlsEvent::DivertedRawWheel {
            resolution,
            delta_vertical,
            ..
        } => Some(RawWheelEvent {
            resolution,
            delta: delta_vertical,
        }),
        ReprogControlsEvent::DivertedButtons(_)
        | ReprogControlsEvent::DivertedRawMouseXy { .. }
        | ReprogControlsEvent::AnalyticsKeyEvents(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use hidpp::{nibble::U4, protocol::v20};

    use super::*;
    use crate::reprog_controls::GESTURE_BUTTON_CID;

    fn event(function_id: u8, software_id: u8, payload: [u8; 16]) -> v20::Message {
        v20::Message::Long(
            v20::MessageHeader {
                device_index: 2,
                feature_index: 7,
                function_id: U4::from_lo(function_id),
                software_id: U4::from_lo(software_id),
            },
            payload,
        )
    }

    #[test]
    fn decodes_diverted_buttons() {
        let mut p = [0u8; 16];
        p[0..2].copy_from_slice(&GESTURE_BUTTON_CID.to_be_bytes());
        let decoded = decode_event(&event(0, 0, p), 2, 7);
        assert_eq!(
            decoded,
            Some(RawControlEvent::DivertedButtons([
                GESTURE_BUTTON_CID,
                0,
                0,
                0,
            ]))
        );
        assert!(decoded.is_some_and(|e| e.is_pressed(GESTURE_BUTTON_CID)));
    }

    #[test]
    fn decodes_signed_raw_xy() {
        let mut p = [0u8; 16];
        p[0..2].copy_from_slice(&(-5i16).to_be_bytes());
        p[2..4].copy_from_slice(&12i16.to_be_bytes());
        assert_eq!(
            decode_event(&event(1, 0, p), 2, 7),
            Some(RawControlEvent::RawXy { dx: -5, dy: 12 })
        );
    }

    #[test]
    fn ignores_responses_and_foreign_messages() {
        let p = [0u8; 16];
        // software_id != 0 marks a request response, not an event.
        assert_eq!(decode_event(&event(0, 5, p), 2, 7), None);
        // Right device + feature, but an event outside the gesture-control path.
        assert_eq!(decode_event(&event(2, 0, p), 2, 7), None);
        // Wrong feature index.
        assert_eq!(decode_event(&event(0, 0, p), 2, 9), None);
    }

    #[test]
    fn decodes_signed_raw_wheel_in_both_resolutions() {
        let mut low = [0u8; 16];
        low[0] = 0x01;
        low[1..3].copy_from_slice(&(-3i16).to_be_bytes());
        assert_eq!(
            decode_raw_wheel_event(&event(4, 0, low), 2, 7),
            Some(RawWheelEvent {
                resolution: RawWheelResolution::Low,
                delta: -3,
            })
        );
        let mut high = [0u8; 16];
        high[0] = 0x11;
        high[1..3].copy_from_slice(&24i16.to_be_bytes());
        assert_eq!(
            decode_raw_wheel_event(&event(4, 0, high), 2, 7),
            Some(RawWheelEvent {
                resolution: RawWheelResolution::High,
                delta: 24,
            })
        );
        // ...and the gesture-pipeline decoder leaves it alone.
        assert_eq!(decode_event(&event(4, 0, high), 2, 7), None);
    }

    #[test]
    fn raw_wheel_decoding_ignores_other_events_and_responses() {
        let mut p = [0u8; 16];
        p[0..2].copy_from_slice(&GESTURE_BUTTON_CID.to_be_bytes());
        assert_eq!(decode_raw_wheel_event(&event(0, 0, p), 2, 7), None);
        assert_eq!(decode_raw_wheel_event(&event(1, 0, p), 2, 7), None);
        assert_eq!(decode_raw_wheel_event(&event(4, 5, p), 2, 7), None);
        assert_eq!(decode_raw_wheel_event(&event(4, 0, p), 2, 9), None);
    }
}
