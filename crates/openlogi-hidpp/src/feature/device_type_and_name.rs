//! Implements the `DeviceTypeAndName` feature (ID `0x0005`) that provides some
//! information about the marketing type and name of a device.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{FeatureEndpoint, decode_chunked_name},
    protocol::v20::{self, Hidpp20Error},
};

/// Implements the `DeviceTypeAndName` / `0x0005` feature.
#[derive(Clone, Feature)]
#[creatable(id = 0x0005, version = 0)]
pub struct DeviceTypeAndNameFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,
}

impl DeviceTypeAndNameFeature {
    /// Retrieves the amount of characters in the marketing name of the device.
    pub async fn get_device_name_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Retrieves a chunk of characters of the marketing name of the device,
    /// starting at a specific index (inclusive).
    ///
    /// Depending on the device and channel capabilities, this function will
    /// return at most 3 or 16 characters of the device name.
    ///
    /// Use this function in conjunction with [`Self::get_device_name_count`] to
    /// retrieve the whole device name.\
    /// A convenience wrapper implementing this functionality is provided as
    /// [`Self::get_whole_device_name`].
    pub async fn get_device_name(&self, index: u8) -> Result<Vec<u8>, Hidpp20Error> {
        let response = self.endpoint.call(1, [index, 0x00, 0x00]).await?;

        match response {
            v20::Message::Long(_, payload) => Ok(payload.to_vec()),
            v20::Message::Short(_, payload) => Ok(payload.to_vec()),
        }
    }

    /// Retrieves the whole marketing name of the device by first calling
    /// [`Self::get_device_name_count`] once and then repeatedly calling
    /// [`Self::get_device_name`] until all characters were received.
    ///
    /// The chunks are joined before decoding: a multi-byte UTF-8 character
    /// can straddle two of them.
    pub async fn get_whole_device_name(&self) -> Result<String, Hidpp20Error> {
        let count = self.get_device_name_count().await?;
        let mut bytes = Vec::with_capacity(count as usize);

        while bytes.len() < count as usize {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the loop condition `bytes.len() < count as usize` bounds the length below count: u8, so it always fits back in a u8"
            )]
            let part = self.get_device_name(bytes.len() as u8).await?;
            bytes.extend_from_slice(&part);
        }

        decode_chunked_name(bytes)
    }

    /// Retrieves the marketing type of the device.
    pub async fn get_device_type(&self) -> Result<DeviceType, Hidpp20Error> {
        DeviceType::try_from(self.endpoint.call(2, [0; 3]).await?.extend_payload()[0])
            .map_err(|_| Hidpp20Error::UnsupportedResponse)
    }
}

/// Represents the type of a HID++2.0 device as returned by the
/// [`DeviceTypeAndNameFeature`] feature.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DeviceType {
    /// Keyboard device.
    Keyboard = 0,
    /// Remote-control device.
    RemoteControl = 1,
    /// Numeric keypad device.
    Numpad = 2,
    /// Mouse device.
    Mouse = 3,
    /// Trackpad device.
    Trackpad = 4,
    /// Trackball device.
    Trackball = 5,
    /// Presenter device.
    Presenter = 6,
    /// Receiver device.
    Receiver = 7,
    /// Headset device.
    Headset = 8,
    /// Webcam device.
    Webcam = 9,
    /// Steering wheel device.
    SteeringWheel = 10,
    /// Joystick device.
    Joystick = 11,
    /// Gamepad device.
    Gamepad = 12,
    /// Dock device.
    Dock = 13,
    /// Speaker device.
    Speaker = 14,
    /// Microphone device.
    Microphone = 15,
    /// Illumination light device.
    IlluminationLight = 16,
    /// Programmable controller device.
    ProgrammableController = 17,
    /// Car-simulator pedals device.
    CarSimPedals = 18,
    /// Adapter device.
    Adapter = 19,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::DeviceTypeAndNameFeature;
    use crate::{
        channel::mock::{MockRawHidChannel, channel_with_reader},
        feature::CreatableFeature,
        nibble::U4,
        protocol::v20::{Message, MessageHeader},
    };

    const DEVICE_INDEX: u8 = 2;
    const FEATURE_INDEX: u8 = 3;

    fn reply(function: u8, head: &[u8]) -> Message {
        let header = MessageHeader {
            device_index: DEVICE_INDEX,
            feature_index: FEATURE_INDEX,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        };
        let mut payload = [0; 16];
        payload[..head.len()].copy_from_slice(head);
        Message::Long(header, payload)
    }

    #[test]
    fn decodes_a_character_split_across_chunks() {
        // "é" is 0xC3 0xA9: its first byte ends the first 16-byte chunk and
        // its second opens the next. Decoding each chunk on its own used to
        // reject the whole name.
        let (name, char_indices) = futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            let feature = DeviceTypeAndNameFeature::new(channel, DEVICE_INDEX, FEATURE_INDEX);
            handle.queue_response(reply(0, &[17]).into());
            handle.queue_response(reply(1, b"Fifteen bytes..\xc3").into());
            handle.queue_response(reply(1, &[0xa9]).into());
            let name = feature.get_whole_device_name().await.unwrap();
            let char_indices: Vec<u8> = handle
                .written_reports()
                .iter()
                .filter(|report| report[3] >> 4 == 1)
                .map(|report| report[4])
                .collect();
            (name, char_indices)
        });

        assert_eq!(name, "Fifteen bytes..é");
        assert_eq!(char_indices, [0, 16]);
    }
}
