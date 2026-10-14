//! Implements the `MouseButtonSpy` feature (ID `0x8110`).
//!
//! Reverse-engineered (libratbag, cvuchener/hidpp), checked on a G502 LIGHTSPEED.

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

const FN_GET_BUTTON_COUNT: u8 = 0;
const FN_START_SPY: u8 = 1;
const FN_STOP_SPY: u8 = 2;
const FN_GET_MAPPING: u8 = 3;
const FN_SET_MAPPING: u8 = 4;

/// Width of the event mask.
pub const MAX_BUTTONS: u8 = 16;

/// Implements the `MouseButtonSpy` / `0x8110` feature.
#[derive(Feature)]
#[creatable(id = 0x8110, version = 0)]
pub struct MouseButtonSpyFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<MouseButtonSpyEvent>,
}

/// An event emitted by [`MouseButtonSpyFeature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum MouseButtonSpyEvent {
    /// Held buttons; bit `n` is slot `n`.
    Buttons {
        /// Held-button mask.
        mask: u16,
    },
}

impl DecodeEvent for MouseButtonSpyEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        (sub_id == 0).then(|| Self::Buttons {
            mask: u16::from_be_bytes([payload[0], payload[1]]),
        })
    }
}

impl MouseButtonSpyEvent {
    /// Decodes one event payload.
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
    }
}

impl MouseButtonSpyFeature {
    /// Number of button slots.
    pub async fn get_button_count(&self) -> Result<u8, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(FN_GET_BUTTON_COUNT, [0; 3])
            .await?
            .extend_payload();
        if payload[0] > MAX_BUTTONS {
            return Err(Hidpp20Error::UnsupportedResponse);
        }
        Ok(payload[0])
    }

    /// Starts button reports.
    pub async fn start_spy(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(FN_START_SPY, [0; 3]).await?;
        Ok(())
    }

    /// Stops button reports.
    pub async fn stop_spy(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(FN_STOP_SPY, [0; 3]).await?;
        Ok(())
    }

    /// The HID button (`0` for none) each slot sends in host mode.
    pub async fn get_mapping(&self, count: u8) -> Result<Vec<u8>, Hidpp20Error> {
        let payload = self
            .endpoint
            .call_long(FN_GET_MAPPING, [0; 16])
            .await?
            .extend_payload();
        mapping_from_payload(&payload, count)
    }

    /// Sets the HID button each slot sends in host mode.
    pub async fn set_mapping(&self, mapping: &[u8]) -> Result<(), Hidpp20Error> {
        self.endpoint
            .call_long(FN_SET_MAPPING, mapping_to_payload(mapping)?)
            .await?;
        Ok(())
    }
}

fn mapping_from_payload(payload: &[u8; 16], count: u8) -> Result<Vec<u8>, Hidpp20Error> {
    let codes = payload
        .get(..usize::from(count))
        .ok_or(Hidpp20Error::UnsupportedResponse)?;
    if codes.iter().any(|&code| code > MAX_BUTTONS) {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    Ok(codes.to_vec())
}

fn mapping_to_payload(mapping: &[u8]) -> Result<[u8; 16], Hidpp20Error> {
    if mapping.len() > usize::from(MAX_BUTTONS) || mapping.iter().any(|&c| c > MAX_BUTTONS) {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    let mut args = [0; 16];
    args[..mapping.len()].copy_from_slice(mapping);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_big_endian_mask() {
        let mut payload = [0; 16];
        payload[..2].copy_from_slice(&[0x01, 0x00]);
        assert_eq!(
            MouseButtonSpyEvent::decode(0, &payload),
            Some(MouseButtonSpyEvent::Buttons { mask: 0x0100 })
        );
        assert_eq!(MouseButtonSpyEvent::decode(1, &payload), None);
    }

    #[test]
    fn mapping_round_trips_and_rejects_bad_codes() {
        let payload = mapping_to_payload(&[1, 2, 3, 4, 5, 0]).unwrap();
        assert_eq!(
            mapping_from_payload(&payload, 6).unwrap(),
            [1, 2, 3, 4, 5, 0]
        );
        mapping_to_payload(&[17]).unwrap_err();
        mapping_to_payload(&[0; 17]).unwrap_err();
        let mut bad = [0; 16];
        bad[0] = 0x20;
        mapping_from_payload(&bad, 1).unwrap_err();
    }
}
