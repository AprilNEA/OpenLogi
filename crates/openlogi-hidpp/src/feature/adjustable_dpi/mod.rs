//! Implements the `AdjustableDpi` feature (ID `0x2201`) that allows reading
//! and changing a mouse sensor's DPI.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

/// Implements the `AdjustableDpi` / `0x2201` feature.
#[derive(Clone)]
pub struct AdjustableDpiFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl CreatableFeature for AdjustableDpiFeature {
    const ID: u16 = 0x2201;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
        }
    }
}

impl Feature for AdjustableDpiFeature {}

impl AdjustableDpiFeature {
    /// Retrieves the number of sensors the device exposes.
    pub async fn get_sensor_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Retrieves the supported DPI values for `sensor_index`.
    ///
    /// `getSensorDpiList` takes the sensor index in the first parameter byte and
    /// returns the whole list in a single long response: the echoed sensor index
    /// followed by up to seven big-endian values, terminated by `0x0000` (the
    /// terminator is absent when the values fill the response). Each value is
    /// either an explicit DPI or a compact range marker (`0xe000 | step`) whose
    /// start is the previous value and whose end is the next value. The returned
    /// list is sorted and deduplicated.
    pub async fn get_sensor_dpi_list(&self, sensor_index: u8) -> Result<Vec<u16>, Hidpp20Error> {
        // Skip the echoed sensor index in byte 0; the DPI values follow.
        let payload = self
            .endpoint
            .call(1, [sensor_index, 0x00, 0x00])
            .await?
            .extend_payload();
        parse_dpi_list_payload(&payload[1..])
    }

    /// Retrieves the currently configured DPI for `sensor_index`.
    pub async fn get_sensor_dpi(&self, sensor_index: u8) -> Result<u16, Hidpp20Error> {
        let payload = self
            .endpoint
            .call(2, [sensor_index, 0x00, 0x00])
            .await?
            .extend_payload();

        Ok(u16::from_be_bytes([payload[1], payload[2]]))
    }

    /// Sets the DPI for `sensor_index`.
    pub async fn set_sensor_dpi(&self, sensor_index: u8, dpi: u16) -> Result<(), Hidpp20Error> {
        let [dpi_hi, dpi_lo] = dpi.to_be_bytes();
        self.endpoint
            .call(3, [sensor_index, dpi_hi, dpi_lo])
            .await?;

        Ok(())
    }
}

fn parse_dpi_list_payload(bytes: &[u8]) -> Result<Vec<u16>, Hidpp20Error> {
    let mut values = Vec::new();
    let mut offset = 0;

    while offset + 1 < bytes.len() {
        let value = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        // `0x0000` terminates the list. A list that fills the whole response
        // has no room for it, so absence of a terminator is not an error — we
        // simply stop when the buffer runs out below.
        if value == 0 {
            break;
        }

        if value >> 13 == 0b111 {
            let step = value & 0x1fff;
            if step == 0 || offset + 3 >= bytes.len() {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            // A range marker's start is the preceding explicit value; a leading
            // marker with no predecessor is malformed.
            let start = u32::from(*values.last().ok_or(Hidpp20Error::UnsupportedResponse)?);
            let last = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]);
            if u32::from(last) < start {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            let mut next = start + u32::from(step);
            while next < u32::from(last) {
                values.push(u16::try_from(next).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
                next += u32::from(step);
            }
            // The high endpoint is always supported, even when it is not an
            // exact multiple of `step` from the low endpoint.
            values.push(last);
            offset += 4;
        } else {
            values.push(value);
            offset += 2;
        }
    }

    if values.is_empty() {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::parse_dpi_list_payload;
    use crate::protocol::v20::Hidpp20Error;

    #[test]
    fn parses_explicit_dpi_list() {
        let payload = [0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0x00, 0x00];

        assert_eq!(parse_dpi_list_payload(&payload).unwrap(), [400, 800, 1600]);
    }

    #[test]
    fn expands_range_encoded_dpi_list() {
        let payload = [0x01, 0x90, 0xe1, 0x90, 0x06, 0x40, 0x00, 0x00];

        assert_eq!(
            parse_dpi_list_payload(&payload).unwrap(),
            [400, 800, 1200, 1600]
        );
    }

    #[test]
    fn sorts_and_deduplicates_values() {
        let payload = [0x06, 0x40, 0x03, 0x20, 0x03, 0x20, 0x00, 0x00];

        assert_eq!(parse_dpi_list_payload(&payload).unwrap(), [800, 1600]);
    }

    #[test]
    fn rejects_range_marker_without_previous_value() {
        let payload = [0xe0, 0x32, 0x1f, 0x40, 0x00, 0x00];

        assert!(matches!(
            parse_dpi_list_payload(&payload),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn rejects_range_marker_without_end_value() {
        let payload = [0x01, 0x90, 0xe0, 0x32];

        assert!(matches!(
            parse_dpi_list_payload(&payload),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn rejects_zero_step_range_marker() {
        let payload = [0x01, 0x90, 0xe0, 0x00, 0x06, 0x40, 0x00, 0x00];

        assert!(matches!(
            parse_dpi_list_payload(&payload),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn rejects_descending_range_marker() {
        let payload = [0x06, 0x40, 0xe0, 0x32, 0x01, 0x90, 0x00, 0x00];

        assert!(matches!(
            parse_dpi_list_payload(&payload),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }

    #[test]
    fn range_keeps_off_grid_high_endpoint() {
        // min 400, step 400, max 1500 — 1500 is not on the 400 grid but is a
        // supported value and must be kept.
        let payload = [0x01, 0x90, 0xe1, 0x90, 0x05, 0xdc, 0x00, 0x00];

        assert_eq!(
            parse_dpi_list_payload(&payload).unwrap(),
            [400, 800, 1200, 1500]
        );
    }

    #[test]
    fn parses_full_list_without_terminator() {
        // A list that fills the response leaves no room for a 0x0000
        // terminator; the values are still valid.
        let payload = [0x01, 0x90, 0x03, 0x20, 0x06, 0x40];

        assert_eq!(parse_dpi_list_payload(&payload).unwrap(), [400, 800, 1600]);
    }

    #[test]
    fn rejects_payload_with_no_values() {
        assert!(matches!(
            parse_dpi_list_payload(&[0x00, 0x00]),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }
}
