//! Implements functionality specific to HID++2.0.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use thiserror::Error;

use crate::{
    channel::{
        AbandonedReply, ChannelError, HidppChannel, HidppMessage, LONG_REPORT_LENGTH,
        SEND_RESPONSE_TIMEOUT, SHORT_REPORT_LENGTH,
    },
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
    #[must_use]
    pub fn header(&self) -> MessageHeader {
        match *self {
            Message::Short(header, _) | Message::Long(header, _) => header,
        }
    }

    /// Extracts the payload of the message and fits it into an array capable of
    /// containing the longest possible payload, filling the rest up with
    /// zeroes.
    #[must_use]
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

    /// The feature error this message reports against a request sent with
    /// `request`, or [`None`] when it is not an error for that request.
    ///
    /// A HID++2.0 error response sets the feature index to `0xFF` and shifts
    /// the request's feature index and function byte one byte to the right,
    /// so an error can be attributed to its request from the header alone —
    /// also for a request written in a report width the channel cannot
    /// correlate, such as the 64-byte `0x12` lighting frame. An error code
    /// this crate does not know decodes as
    /// [`Hidpp20Error::UnsupportedResponse`].
    #[must_use]
    pub fn feature_error_for(&self, request: MessageHeader) -> Option<Hidpp20Error> {
        if !self.is_error_for(request) {
            return None;
        }
        Some(
            ErrorType::try_from(self.extend_payload()[1])
                .map_or(Hidpp20Error::UnsupportedResponse, Hidpp20Error::Feature),
        )
    }

    fn is_error_for(&self, request: MessageHeader) -> bool {
        let header = self.header();
        header.device_index == request.device_index
            && header.feature_index == 0xff
            && nibble::combine(header.function_id, header.software_id) == request.feature_index
            && self.extend_payload()[0] == nibble::combine(request.function_id, request.software_id)
    }
}

impl From<HidppMessage> for Message {
    fn from(msg: HidppMessage) -> Self {
        match msg {
            HidppMessage::Short(payload) => {
                let [_, _, _, rest @ ..] = payload;
                Message::Short(
                    MessageHeader {
                        device_index: payload[0],
                        feature_index: payload[1],
                        function_id: U4::from_hi(payload[2]),
                        software_id: U4::from_lo(payload[2]),
                    },
                    rest,
                )
            }
            HidppMessage::Long(payload) => {
                let [_, _, _, rest @ ..] = payload;
                Message::Long(
                    MessageHeader {
                        device_index: payload[0],
                        feature_index: payload[1],
                        function_id: U4::from_hi(payload[2]),
                        software_id: U4::from_lo(payload[2]),
                    },
                    rest,
                )
            }
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
        self.send_v20_with(msg, AbandonedReply::Quarantine).await
    }

    /// [`Self::send_v20`], choosing what to do about a reply still owed to an
    /// abandoned request with the same header (see [`AbandonedReply`]).
    pub async fn send_v20_with(
        &self,
        msg: Message,
        abandoned: AbandonedReply,
    ) -> Result<Message, Hidpp20Error> {
        let header = msg.header();
        let response = self
            .send_with(
                msg.into(),
                v20_response_predicate(header),
                SEND_RESPONSE_TIMEOUT,
                abandoned,
            )
            .await?;
        decode_v20_response(response)
    }

    /// Sends a HID++2.0 message without timing out its native write, then waits
    /// up to [`SEND_RESPONSE_TIMEOUT`] for the matching response.
    ///
    /// `matches_payload` further filters successful responses where the feature
    /// defines echoed fields. Header-matched errors bypass this filter.
    ///
    /// The caller **must drive this future to completion**. Requester deadlines
    /// belong outside the task that owns this future; see
    /// [`HidppChannel::send_write_through`] for the transport-ordering contract.
    pub async fn send_v20_write_through(
        &self,
        msg: Message,
        matches_payload: impl Fn(&Message) -> bool + Send + 'static,
    ) -> Result<Message, Hidpp20Error> {
        let matches_header = v20_response_predicate(msg.header());
        let response = self
            .send_write_through(
                msg.into(),
                move |raw| {
                    let response = Message::from(*raw);
                    matches_header(raw)
                        && (response.header().feature_index == 0xff || matches_payload(&response))
                },
                SEND_RESPONSE_TIMEOUT,
            )
            .await?;
        decode_v20_response(response)
    }
}

fn v20_response_predicate(
    header: MessageHeader,
) -> impl Fn(&HidppMessage) -> bool + Send + 'static {
    move |&response| {
        let response = Message::from(response);
        response.is_error_for(header) || response.header() == header
    }
}

fn decode_v20_response(response: HidppMessage) -> Result<Message, Hidpp20Error> {
    let response = Message::from(response);
    if response.header().feature_index == 0xff {
        let err = ErrorType::try_from(response.extend_payload()[1])
            .map_err(|_| Hidpp20Error::UnsupportedResponse)?;

        return Err(Hidpp20Error::Feature(err));
    }

    Ok(response)
}

/// Represents the type of an error a HID++2.0 device returns if a feature
/// function fails.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ErrorType {
    /// No error.
    NoError = 0,
    /// Unknown error.
    Unknown = 1,
    /// Invalid argument.
    InvalidArgument = 2,
    /// Argument out of range.
    OutOfRange = 3,
    /// Hardware error.
    HwError = 4,
    /// Logitech-internal firmware error.
    LogitechInternal = 5,
    /// Invalid feature index.
    InvalidFeatureIndex = 6,
    /// Invalid function ID.
    InvalidFunctionId = 7,
    /// Device is busy.
    Busy = 8,
    /// Operation is unsupported.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A request to function 3 of the feature at index `0x07`, software id
    /// `0xa`, on device `0x01`.
    fn request() -> MessageHeader {
        MessageHeader {
            device_index: 0x01,
            feature_index: 0x07,
            function_id: U4::from_lo(0x3),
            software_id: U4::from_lo(0xa),
        }
    }

    /// A short report `[device, feature, function byte, payload..]`.
    fn short(bytes: [u8; SHORT_REPORT_LENGTH - 1]) -> Message {
        Message::from(HidppMessage::Short(bytes))
    }

    #[test]
    fn an_error_naming_the_request_is_attributed_to_it() {
        let error = short([0x01, 0xff, 0x07, 0x3a, 0x02, 0x00]);
        assert!(matches!(
            error.feature_error_for(request()),
            Some(Hidpp20Error::Feature(ErrorType::InvalidArgument))
        ));
    }

    #[test]
    fn an_error_for_another_request_is_not() {
        // Another function of the same feature, another feature, another
        // device, and the request's own success reply.
        for other in [
            [0x01, 0xff, 0x07, 0x5a, 0x02, 0x00],
            [0x01, 0xff, 0x08, 0x3a, 0x02, 0x00],
            [0x02, 0xff, 0x07, 0x3a, 0x02, 0x00],
            [0x01, 0x07, 0x3a, 0x00, 0x00, 0x00],
        ] {
            assert!(
                short(other).feature_error_for(request()).is_none(),
                "{other:02x?}"
            );
        }
    }

    #[test]
    fn an_unknown_error_code_is_an_unsupported_response() {
        let error = short([0x01, 0xff, 0x07, 0x3a, 0x7f, 0x00]);
        assert!(matches!(
            error.feature_error_for(request()),
            Some(Hidpp20Error::UnsupportedResponse)
        ));
    }
}
