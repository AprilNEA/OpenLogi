//! Implements the `AdjustableDpi` feature (ID `0x2201`) that allows reading
//! and changing a mouse sensor's DPI.

use std::sync::Arc;

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `AdjustableDpi` / `0x2201` feature.
#[derive(Clone)]
pub struct AdjustableDpiFeature {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The index of the device to implement the feature for.
    device_index: u8,

    /// The index of the feature in the feature table.
    feature_index: u8,
}

impl CreatableFeature for AdjustableDpiFeature {
    const ID: u16 = 0x2201;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }
}

impl Feature for AdjustableDpiFeature {}

impl AdjustableDpiFeature {
    /// Retrieves the number of sensors the device exposes.
    pub async fn get_sensor_count(&self) -> Result<u8, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(0),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, 0x00],
            ))
            .await?;

        Ok(response.extend_payload()[0])
    }

    /// Retrieves the supported DPI values for `sensor_index`.
    ///
    /// The device may return either explicit two-byte DPI values or a compact
    /// range marker (`0xe000 | step`) followed by the range end. The returned
    /// list is sorted and deduplicated.
    pub async fn get_sensor_dpi_list(&self, sensor_index: u8) -> Result<Vec<u16>, Hidpp20Error> {
        let mut dpi_bytes = Vec::new();

        for page in 0..=u8::MAX {
            let response = self
                .chan
                .send_v20(v20::Message::Short(
                    v20::MessageHeader {
                        device_index: self.device_index,
                        feature_index: self.feature_index,
                        function_id: U4::from_lo(1),
                        software_id: self.chan.get_sw_id(),
                    },
                    [0x00, sensor_index, page],
                ))
                .await?;

            let payload = response.extend_payload();
            dpi_bytes.extend_from_slice(&payload[1..]);

            if dpi_bytes
                .windows(2)
                .last()
                .is_some_and(|bytes| bytes == [0x00, 0x00])
            {
                break;
            }
        }

        parse_dpi_list_payload(&dpi_bytes)
    }

    /// Retrieves the currently configured DPI for `sensor_index`.
    pub async fn get_sensor_dpi(&self, sensor_index: u8) -> Result<u16, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(2),
                    software_id: self.chan.get_sw_id(),
                },
                [sensor_index, 0x00, 0x00],
            ))
            .await?;
        let payload = response.extend_payload();

        Ok(u16::from_be_bytes([payload[1], payload[2]]))
    }

    /// Sets the DPI for `sensor_index`.
    pub async fn set_sensor_dpi(&self, sensor_index: u8, dpi: u16) -> Result<(), Hidpp20Error> {
        let [dpi_hi, dpi_lo] = dpi.to_be_bytes();
        let _ = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(3),
                    software_id: self.chan.get_sw_id(),
                },
                [sensor_index, dpi_hi, dpi_lo],
            ))
            .await?;

        Ok(())
    }
}

fn parse_dpi_list_payload(bytes: &[u8]) -> Result<Vec<u16>, Hidpp20Error> {
    let mut values = Vec::new();
    let mut offset = 0;

    while offset + 1 < bytes.len() {
        let value = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
        if value == 0 {
            values.sort_unstable();
            values.dedup();
            return Ok(values);
        }

        if value >> 13 == 0b111 {
            let step = value & 0x1fff;
            if step == 0 || values.is_empty() || offset + 3 >= bytes.len() {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            let last = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]);
            let mut next = u32::from(*values.last().ok_or(Hidpp20Error::UnsupportedResponse)?)
                + u32::from(step);
            if next > u32::from(last) {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
            while next <= u32::from(last) {
                values.push(u16::try_from(next).map_err(|_| Hidpp20Error::UnsupportedResponse)?);
                next += u32::from(step);
            }
            offset += 4;
        } else {
            values.push(value);
            offset += 2;
        }
    }

    Err(Hidpp20Error::UnsupportedResponse)
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
    fn rejects_payload_without_terminator() {
        let payload = [0x01, 0x90, 0x03, 0x20];

        assert!(matches!(
            parse_dpi_list_payload(&payload),
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    }
}
