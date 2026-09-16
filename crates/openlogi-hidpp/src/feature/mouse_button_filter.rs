//! Implements the `MouseButtonFilter` feature (ID `0x8110`).
//!
//! The function IDs, mapping byte values, and event bitmask layout are
//! reverse-engineered from the public descriptions of cvuchener/hidpp
//! `IMouseButtonSpy` and libratbag `HIDPP_PAGE_MOUSE_BUTTON_SPY`. Logitech has
//! not published this feature in the public HID++ specs used by this crate, so
//! additions must be verified against hardware rather than guessed. This file
//! does not copy either project's code.
//!
//! `SetMouseButtonMapping` only changes standard HID reports, and only while
//! the device is in Host mode (`0x8100`). Hardware-specific button-name maps
//! are out of scope.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// HID++ 2.0 function ids for `0x8110` (4-bit). Reverse-engineered; see the
/// module docs.
const FN_GET_MOUSE_BUTTON_COUNT: u8 = 0;
const FN_START_MOUSE_BUTTON_SPY: u8 = 1;
const FN_STOP_MOUSE_BUTTON_SPY: u8 = 2;
const FN_GET_MOUSE_BUTTON_MAPPING: u8 = 3;
const FN_SET_MOUSE_BUTTON_MAPPING: u8 = 4;

/// Event sub-id for the unsolicited button bitmask. Reverse-engineered; see
/// the module docs.
const EVENT_MOUSE_BUTTON: u8 = 0;

/// Highest HID button code the mapping may enable (`1..=16`). `0` disables a
/// slot. The spy event is a 16-bit mask, so the device cannot advertise more
/// than 16 buttons.
const MAX_BUTTON_COUNT: u8 = 16;

/// Implements the `MouseButtonFilter` / `0x8110` feature (Mouse Button Spy).
#[derive(Feature)]
#[creatable(id = 0x8110, version = 0)]
pub struct MouseButtonFilterFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<MouseButtonFilterEvent>,
}

impl DecodeEvent for MouseButtonFilterEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        if sub_id != EVENT_MOUSE_BUTTON {
            return None;
        }

        Some(Self::Buttons {
            mask: u16::from_be_bytes([payload[0], payload[1]]),
        })
    }
}

impl MouseButtonFilterFeature {
    /// Number of spy-button slots the device reports.
    pub async fn get_mouse_button_count(&self) -> Result<u8, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_MOUSE_BUTTON_COUNT, [0; 3])
            .await?
            .extend_payload();
        mouse_button_count_from_payload(&payload)
    }

    /// Starts unsolicited [`MouseButtonFilterEvent::Buttons`] reports.
    ///
    /// The device keeps streaming until [`Self::stop_mouse_button_spy`] or a
    /// power cycle. Callers that enable the spy must stop it on shutdown.
    pub async fn start_mouse_button_spy(&self) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call(FN_START_MOUSE_BUTTON_SPY, [0; 3])
            .await?;
        Ok(())
    }

    /// Stops unsolicited button reports started by [`Self::start_mouse_button_spy`].
    pub async fn stop_mouse_button_spy(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(FN_STOP_MOUSE_BUTTON_SPY, [0; 3]).await?;
        Ok(())
    }

    /// Current HID mapping for each spy-button slot.
    ///
    /// Each byte is `0` (disabled in standard HID reports) or a code in
    /// `1..=16`. Length equals [`Self::get_mouse_button_count`]. The spy event
    /// bitmask is independent of this mapping.
    ///
    /// Uses a long report so all 16 mapping bytes can come back in one reply.
    pub async fn get_mouse_button_mapping(&self) -> Result<Vec<u8>, Hidpp20Error> {
        let count = self.get_mouse_button_count().await?;
        let payload = self
            .endpoint
            .call_long(FN_GET_MOUSE_BUTTON_MAPPING, [0; 16])
            .await?
            .extend_payload();
        mouse_button_mapping_from_payload(&payload, count)
    }

    /// Writes the HID mapping for each spy-button slot.
    ///
    /// Each byte is `0` (disabled in standard HID reports) or a code in
    /// `1..=16`. Length must equal [`Self::get_mouse_button_count`]. The spy
    /// event bitmask is independent of this mapping. The write applies only
    /// in Host mode; onboard profiles ignore it.
    pub async fn set_mouse_button_mapping(&self, mapping: &[u8]) -> Result<(), Hidpp20Error> {
        let count = self.get_mouse_button_count().await?;
        let args = mouse_button_mapping_to_payload(mapping, count)?;
        self.endpoint
            .call_long(FN_SET_MOUSE_BUTTON_MAPPING, args)
            .await?;
        Ok(())
    }
}

/// Represents an event emitted by the [`MouseButtonFilterFeature`] feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum MouseButtonFilterEvent {
    /// Bitmask of buttons held when the spy is running (event sub-id 0).
    ///
    /// Bit 0 is the first spy-button slot. The mask is big-endian on the wire.
    Buttons {
        /// Pressed-button bitmask.
        mask: u16,
    },
}

fn mouse_button_count_from_payload(payload: &[u8; 16]) -> Result<u8, Hidpp20Error> {
    let count = payload[0];
    if count > MAX_BUTTON_COUNT {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(count)
}

fn mouse_button_mapping_from_payload(
    payload: &[u8; 16],
    count: u8,
) -> Result<Vec<u8>, Hidpp20Error> {
    let n = usize::from(count);
    if n > payload.len() {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    let codes = payload[..n].to_vec();
    if codes.iter().any(|&code| code > MAX_BUTTON_COUNT) {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(codes)
}

fn mouse_button_mapping_to_payload(mapping: &[u8], count: u8) -> Result<[u8; 16], Hidpp20Error> {
    let n = usize::from(count);
    if mapping.len() != n || n > 16 {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    if mapping.iter().any(|&code| code > MAX_BUTTON_COUNT) {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    let mut args = [0; 16];
    args[..n].copy_from_slice(mapping);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::{
        MouseButtonFilterEvent, mouse_button_count_from_payload, mouse_button_mapping_from_payload,
        mouse_button_mapping_to_payload,
    };
    use crate::feature::DecodeEvent;
    use crate::protocol::v20::Hidpp20Error;

    fn payload_with_prefix(bytes: &[u8]) -> [u8; 16] {
        let mut payload = [0; 16];
        payload[..bytes.len()].copy_from_slice(bytes);
        payload
    }

    #[test]
    fn decodes_button_mask_bits_zero_and_two() {
        let payload = payload_with_prefix(&[0x00, 0x05]);
        let MouseButtonFilterEvent::Buttons { mask } =
            MouseButtonFilterEvent::decode(0, &payload).expect("sub-id 0 is the button event");

        assert_eq!(mask, 0x0005);
        assert_eq!(mask & (1 << 0), 1 << 0);
        assert_eq!(mask & (1 << 1), 0);
        assert_eq!(mask & (1 << 2), 1 << 2);
    }

    #[test]
    fn decodes_high_byte_of_button_mask() {
        let payload = payload_with_prefix(&[0x80, 0x00]);
        let MouseButtonFilterEvent::Buttons { mask } =
            MouseButtonFilterEvent::decode(0, &payload).expect("sub-id 0 is the button event");

        assert_eq!(mask, 0x8000);
        assert_eq!(mask & (1 << 15), 1 << 15);
    }

    #[test]
    fn ignores_unknown_event_sub_id() {
        assert!(MouseButtonFilterEvent::decode(1, &[0; 16]).is_none());
    }

    #[test]
    fn parses_button_count() {
        let payload = payload_with_prefix(&[0x0b]);
        assert_eq!(mouse_button_count_from_payload(&payload).unwrap(), 11);
    }

    #[test]
    fn rejects_button_count_above_sixteen() {
        let payload = payload_with_prefix(&[0x11]);
        assert!(matches!(
            mouse_button_count_from_payload(&payload),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn parses_mapping_codes_and_disabled_slots() {
        let payload = payload_with_prefix(&[1, 2, 0, 16]);
        assert_eq!(
            mouse_button_mapping_from_payload(&payload, 4).unwrap(),
            [1, 2, 0, 16]
        );
    }

    #[test]
    fn rejects_mapping_code_above_sixteen() {
        let payload = payload_with_prefix(&[1, 17]);
        assert!(matches!(
            mouse_button_mapping_from_payload(&payload, 2),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn encodes_mapping_into_long_payload() {
        let args = mouse_button_mapping_to_payload(&[1, 2, 0, 16], 4).unwrap();
        assert_eq!(&args[..4], &[1, 2, 0, 16]);
        assert!(args[4..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn rejects_mapping_length_mismatch() {
        assert!(matches!(
            mouse_button_mapping_to_payload(&[1, 2], 3),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn rejects_encoded_mapping_code_above_sixteen() {
        assert!(matches!(
            mouse_button_mapping_to_payload(&[1, 17], 2),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }
}
