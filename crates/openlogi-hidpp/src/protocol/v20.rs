//! Implements functionality specific to HID++2.0.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use thiserror::Error;

use crate::{
    channel::{ChannelError, HidppChannel, HidppMessage, LONG_REPORT_LENGTH, SHORT_REPORT_LENGTH},
    nibble::{self, U4},
};

/// Represents the header that every [`HidppMessage`] of HID++2.0 starts with.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MessageHeader {
    /// The index of the device involved in the communication.
    pub device_index: u8,

    /// The index of the feature the message belongs to.
    ///
    /// This is not the same as the feature ID, but the index returned from a
    /// feature enumeration request.
    pub feature_index: u8,

    /// The ID of the function involved in the communication.
    pub function_id: U4,

    /// The ID of the software communicating with the device.
    pub software_id: U4,
}

/// Represents a HID++2.0 message.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Message {
    /// Represents a short HID++2.0 message with 3 bytes of payload.
    Short(MessageHeader, [u8; SHORT_REPORT_LENGTH - 4]),

    /// Represents a long HID++2.0 message with 16 bytes of payload.
    Long(MessageHeader, [u8; LONG_REPORT_LENGTH - 4]),
}

impl Message {
    /// Extracts the header of the message.
    pub fn header(&self) -> MessageHeader {
        match *self {
            Message::Short(header, _) => header,
            Message::Long(header, _) => header,
        }
    }

    /// Extracts the payload of the message and fits it into an array capable of
    /// containing the longest possible payload, filling the rest up with
    /// zeroes.
    pub fn extend_payload(&self) -> [u8; LONG_REPORT_LENGTH - 4] {
        match *self {
            Message::Short(_, payload) => {
                let mut data = [0; LONG_REPORT_LENGTH - 4];
                data[..SHORT_REPORT_LENGTH - 4].copy_from_slice(&payload);
                data
            }
            Message::Long(_, payload) => payload,
        }
    }
}

impl From<HidppMessage> for Message {
    fn from(msg: HidppMessage) -> Self {
        match msg {
            HidppMessage::Short(payload) => Message::Short(
                MessageHeader {
                    device_index: payload[0],
                    feature_index: payload[1],
                    function_id: U4::from_hi(payload[2]),
                    software_id: U4::from_lo(payload[2]),
                },
                payload[3..].try_into().unwrap(),
            ),
            HidppMessage::Long(payload) => Message::Long(
                MessageHeader {
                    device_index: payload[0],
                    feature_index: payload[1],
                    function_id: U4::from_hi(payload[2]),
                    software_id: U4::from_lo(payload[2]),
                },
                payload[3..].try_into().unwrap(),
            ),
        }
    }
}

impl From<Message> for HidppMessage {
    fn from(msg: Message) -> Self {
        match msg {
            Message::Short(header, payload) => {
                let mut data = [0u8; SHORT_REPORT_LENGTH - 1];
                data[0] = header.device_index;
                data[1] = header.feature_index;
                data[2] = nibble::combine(header.function_id, header.software_id);
                data[3..].copy_from_slice(&payload);

                HidppMessage::Short(data)
            }
            Message::Long(header, payload) => {
                let mut data = [0u8; LONG_REPORT_LENGTH - 1];
                data[0] = header.device_index;
                data[1] = header.feature_index;
                data[2] = nibble::combine(header.function_id, header.software_id);
                data[3..].copy_from_slice(&payload);

                HidppMessage::Long(data)
            }
        }
    }
}

impl HidppChannel {
    /// Sends a HID++2.0 message across the channel and waits for a response
    /// that matches the message header.
    ///
    /// This method simply calls [`Self::send`] with a pre-built response
    /// predicate comparing the headers of the outgoing and incoming message.
    pub async fn send_v20(&self, msg: Message) -> Result<Message, Hidpp20Error> {
        let header = msg.header();

        let response = Message::from(
            self.send(msg.into(), move |&response| {
                let resp_msg = Message::from(response);
                let resp_header = resp_msg.header();

                // A HID++2.0 error response sets the feature index to 0xFF and moves all header
                // values starting from the real feature index one byte to the right.
                let is_error = resp_header.device_index == header.device_index
                    && resp_header.feature_index == 0xff
                    && nibble::combine(resp_header.function_id, resp_header.software_id)
                        == header.feature_index
                    && resp_msg.extend_payload()[0]
                        == nibble::combine(header.function_id, header.software_id);

                is_error || resp_header == header
            })
            .await?,
        );

        if response.header().feature_index == 0xff {
            let err = ErrorType::try_from(response.extend_payload()[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?;

            return Err(Hidpp20Error::Feature(err));
        }

        Ok(response)
    }
}

/// Represents the type of an error a HID++2.0 device returns if a feature
/// function fails.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ErrorType {
    NoError = 0,
    Unknown = 1,
    InvalidArgument = 2,
    OutOfRange = 3,
    HwError = 4,
    LogitechInternal = 5,
    InvalidFeatureIndex = 6,
    InvalidFunctionId = 7,
    Busy = 8,
    Unsupported = 9,
}

/// Represents an error that may occur when calling a HID++2.0 feature function.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Hidpp20Error {
    /// Indicates that an error occurred while communicating across the HID++
    /// channel.
    #[error("the HID++ channel returned an error")]
    Channel(#[from] ChannelError),

    /// Indicates that a call to a HID++2.0 feature function resulted in an
    /// error.
    #[error("a HID++2.0 feature returned an error")]
    Feature(ErrorType),

    /// Indicates that a received response is not fully supported.
    #[error("the received response from the device is (partly) unsupported")]
    UnsupportedResponse,
}
