use hidpp::protocol::v20;

use super::{AnalyticsKeyEvent, ControlId, ReprogControlsEvent, decode_full_event};

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
    /// `analyticsKeyEvents`: up to five `(cid, event)` entries from controls
    /// armed for analytics reporting — the MX Master 4 Action Ring pad's event
    /// path (`event` `0x01` = press, `0x00` = release). A zero CID is an empty
    /// slot.
    AnalyticsKeys([AnalyticsKeyEvent; 5]),
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
            ReprogControlsEvent::AnalyticsKeyEvents(entries) => Ok(Self::AnalyticsKeys(entries)),
            ReprogControlsEvent::DivertedRawWheel { .. } => Err(event),
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
    fn decodes_analytics_press_and_release_entries() {
        let mut p = [0u8; 16];
        // Entry 0: Action Ring pad press; entry 1: companion CID release.
        p[0..2].copy_from_slice(&crate::reprog_controls::ACTION_RING_CID.to_be_bytes());
        p[2] = 0x01;
        p[3..5].copy_from_slice(&0x0050u16.to_be_bytes());
        p[5] = 0x00;
        let decoded = decode_event(&event(2, 0, p), 2, 7);
        let Some(RawControlEvent::AnalyticsKeys(entries)) = decoded else {
            panic!("analytics key events should decode, got {decoded:?}");
        };
        assert_eq!(
            entries[0],
            AnalyticsKeyEvent {
                cid: ControlId(crate::reprog_controls::ACTION_RING_CID),
                event: 0x01,
            },
            "press entries carry event 0x01"
        );
        assert_eq!(
            entries[1],
            AnalyticsKeyEvent {
                cid: ControlId(0x0050),
                event: 0x00,
            },
            "release entries carry event 0x00"
        );
        assert_eq!(
            entries[2],
            AnalyticsKeyEvent::default(),
            "unused slots decode as zero CIDs"
        );
    }

    #[test]
    fn ignores_responses_and_foreign_messages() {
        let p = [0u8; 16];
        // software_id != 0 marks a request response, not an event.
        assert_eq!(decode_event(&event(0, 5, p), 2, 7), None);
        // Right device + feature, but an event outside the gesture-control
        // path (function 4 = raw wheel, which OpenLogi discards).
        assert_eq!(decode_event(&event(4, 0, p), 2, 7), None);
        // Wrong feature index.
        assert_eq!(decode_event(&event(0, 0, p), 2, 9), None);
    }
}
