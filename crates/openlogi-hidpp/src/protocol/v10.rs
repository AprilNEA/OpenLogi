//! Implements functionality specific to HID++1.0.

use num_enum::{IntoPrimitive, TryFromPrimitive};
use thiserror::Error;

use crate::channel::{
    ChannelError, HidppChannel, HidppMessage, LONG_REPORT_LENGTH, SHORT_REPORT_LENGTH,
};

/// Represents the header that every [`HidppMessage`] of HID++1.0 starts with.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MessageHeader {
    /// The index of the device involved in the communication.
    pub device_index: u8,

    /// The sub ID of the message.
    pub sub_id: u8,
}

/// Represents a HID++1.0 message.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Message {
    /// Represents a short HID++1.0 message with 4 bytes of payload.
    Short(MessageHeader, [u8; SHORT_REPORT_LENGTH - 3]),

    /// Represents a long HID++1.0 message with 17 bytes of payload.
    Long(MessageHeader, [u8; LONG_REPORT_LENGTH - 3]),
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
    pub fn extend_payload(&self) -> [u8; LONG_REPORT_LENGTH - 3] {
        match *self {
            Message::Short(_, payload) => {
                let mut data = [0; LONG_REPORT_LENGTH - 3];
                data[..SHORT_REPORT_LENGTH - 3].copy_from_slice(&payload);
                data
            }
            Message::Long(_, payload) => payload,
        }
    }
}

impl From<HidppMessage> for Message {
    fn from(msg: HidppMessage) -> Self {
        match msg {
            HidppMessage::Short(payload) => {
                let [_, _, rest @ ..] = payload;
                Message::Short(
                    MessageHeader {
                        device_index: payload[0],
                        sub_id: payload[1],
                    },
                    rest,
                )
            }
            HidppMessage::Long(payload) => {
                let [_, _, rest @ ..] = payload;
                Message::Long(
                    MessageHeader {
                        device_index: payload[0],
                        sub_id: payload[1],
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
                data[1] = header.sub_id;
                data[2..].copy_from_slice(&payload);

                HidppMessage::Short(data)
            }
            Message::Long(header, payload) => {
                let mut data = [0u8; LONG_REPORT_LENGTH - 1];
                data[0] = header.device_index;
                data[1] = header.sub_id;
                data[2..].copy_from_slice(&payload);

                HidppMessage::Long(data)
            }
        }
    }
}

/// Whether `msg` answers the RAP request `(device, msg_type, address)`.
///
/// `echo` narrows the match to replies whose first data byte repeats that
/// value. Sub-register reads need it: every slot of the receiver's `0xB5`
/// register is asked through the same `(device, sub id, address)` header and
/// differs only in the sub-register byte, which the receiver repeats as the
/// first byte of its reply. Without the check, one slot's read is satisfied by
/// any other slot's reply — including one requested by another process sharing
/// the node, which is how a phantom "slot 3" appeared in inventories whenever
/// two OpenLogi processes probed a Bolt receiver at once. Error replies carry
/// no data byte and are matched on the header alone.
fn is_rap_response(
    device: u8,
    msg_type: MessageType,
    address: u8,
    echo: Option<u8>,
    msg: &HidppMessage,
) -> bool {
    let raw: [u8; 4] = match msg {
        HidppMessage::Short(d) => [d[0], d[1], d[2], d[3]],
        HidppMessage::Long(d) => [d[0], d[1], d[2], d[3]],
    };

    raw[0] == device
        && ((raw[1] == msg_type.into()
            && raw[2] == address
            && echo.is_none_or(|echo| raw[3] == echo))
            || (raw[1] == MessageType::Error.into()
                && raw[2] == msg_type.into()
                && raw[3] == address))
}

impl HidppChannel {
    /// Reads the data from a short 3-byte register using HID++1.0/RAP.
    pub async fn read_register(
        &self,
        device: u8,
        address: u8,
        parameters: [u8; 3],
    ) -> Result<[u8; 3], Hidpp10Error> {
        let mut data = [address, 0x00, 0x00, 0x00];
        data[1..].copy_from_slice(&parameters);

        let response = Message::from(
            self.send(
                Message::Short(
                    MessageHeader {
                        device_index: device,
                        sub_id: MessageType::GetRegister.into(),
                    },
                    data,
                )
                .into(),
                move |raw| is_rap_response(device, MessageType::GetRegister, address, None, raw),
            )
            .await?,
        );

        let payload = response.extend_payload();

        if response.header().sub_id == MessageType::Error.into() {
            let err =
                ErrorType::try_from(payload[2]).map_err(|_| Hidpp10Error::UnsupportedResponse)?;

            return Err(Hidpp10Error::RegisterAccess(err));
        }

        let [_, p1, p2, p3, ..] = payload;
        Ok([p1, p2, p3])
    }

    /// Writes data to a short 3-byte register using HID++1.0/RAP.
    pub async fn write_register(
        &self,
        device: u8,
        address: u8,
        payload: [u8; 3],
    ) -> Result<(), Hidpp10Error> {
        let mut data = [address, 0x00, 0x00, 0x00];
        data[1..].copy_from_slice(&payload);

        let response = Message::from(
            self.send(
                Message::Short(
                    MessageHeader {
                        device_index: device,
                        sub_id: MessageType::SetRegister.into(),
                    },
                    data,
                )
                .into(),
                move |raw| is_rap_response(device, MessageType::SetRegister, address, None, raw),
            )
            .await?,
        );

        if response.header().sub_id == MessageType::Error.into() {
            let err = ErrorType::try_from(response.extend_payload()[2])
                .map_err(|_| Hidpp10Error::UnsupportedResponse)?;

            return Err(Hidpp10Error::RegisterAccess(err));
        }

        Ok(())
    }

    /// Reads the data from a long 16-byte register using HID++1.0/RAP.
    pub async fn read_long_register(
        &self,
        device: u8,
        address: u8,
        parameters: [u8; 3],
    ) -> Result<[u8; 16], Hidpp10Error> {
        self.read_long_register_matching(device, address, parameters, None)
            .await
    }

    /// Reads one sub-register of a long register using HID++1.0/RAP.
    ///
    /// `sub_register` goes out as the first parameter, and only a reply that
    /// repeats it as its first data byte is accepted. The receiver's
    /// pairing-information register (`0xB5`) keys every paired slot through
    /// one address, so the header alone cannot tell the slots' replies apart —
    /// see [`is_rap_response`]. The returned data starts with the echoed byte,
    /// the same layout [`Self::read_long_register`] returns.
    pub async fn read_long_sub_register(
        &self,
        device: u8,
        address: u8,
        sub_register: u8,
        parameters: [u8; 2],
    ) -> Result<[u8; 16], Hidpp10Error> {
        self.read_long_register_matching(
            device,
            address,
            [sub_register, parameters[0], parameters[1]],
            Some(sub_register),
        )
        .await
    }

    async fn read_long_register_matching(
        &self,
        device: u8,
        address: u8,
        parameters: [u8; 3],
        echo: Option<u8>,
    ) -> Result<[u8; 16], Hidpp10Error> {
        let mut data = [address, 0x00, 0x00, 0x00];
        data[1..].copy_from_slice(&parameters);

        let response = Message::from(
            self.send(
                Message::Short(
                    MessageHeader {
                        device_index: device,
                        sub_id: MessageType::GetLongRegister.into(),
                    },
                    data,
                )
                .into(),
                move |raw| {
                    is_rap_response(device, MessageType::GetLongRegister, address, echo, raw)
                },
            )
            .await?,
        );

        let payload = response.extend_payload();

        if response.header().sub_id == MessageType::Error.into() {
            let err =
                ErrorType::try_from(payload[2]).map_err(|_| Hidpp10Error::UnsupportedResponse)?;

            return Err(Hidpp10Error::RegisterAccess(err));
        }

        let [_, rest @ ..] = payload;
        Ok(rest)
    }

    /// Writes data to a long 16-byte register using HID++1.0/RAP.
    pub async fn write_long_register(
        &self,
        device: u8,
        address: u8,
        payload: [u8; 16],
    ) -> Result<(), Hidpp10Error> {
        let mut data = [0u8; 17];
        data[0] = address;
        data[1..].copy_from_slice(&payload);

        let response = Message::from(
            self.send(
                Message::Long(
                    MessageHeader {
                        device_index: device,
                        sub_id: MessageType::SetLongRegister.into(),
                    },
                    data,
                )
                .into(),
                move |raw| {
                    is_rap_response(device, MessageType::SetLongRegister, address, None, raw)
                },
            )
            .await?,
        );

        if response.header().sub_id == MessageType::Error.into() {
            let err = ErrorType::try_from(response.extend_payload()[2])
                .map_err(|_| Hidpp10Error::UnsupportedResponse)?;

            return Err(Hidpp10Error::RegisterAccess(err));
        }

        Ok(())
    }
}

/// Represents a globally defined sub ID of a HID++1.0 message.
///
/// This enum only includes sub IDs that are defined globally across all
/// devices. Most devices (e.g. the Unifying Receiver) define additional sub IDs
/// specific to their functionality.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum MessageType {
    /// Used to set a 3-byte register value. A sent message of this type is
    /// usually responded with a response message of the same type (or
    /// [`Self::Error`]).
    SetRegister = 0x80,

    /// Used to retrieve a 3-byte register value. A sent message of this type is
    /// usually responded with a response message of the same type (or
    /// [`Self::Error`]).
    GetRegister = 0x81,

    /// Used to set a 16-byte register value. A sent message of this type is
    /// usually responded with a response message of the same type (or
    /// [`Self::Error`]).
    SetLongRegister = 0x82,

    /// Used to retrieve a 16-byte register value. A sent message of this type
    /// is usually responded with a response message of the same type (or
    /// [`Self::Error`]).
    GetLongRegister = 0x83,

    /// Used to indicate an error response. The error code usually included in
    /// the message can be mapped using [`ErrorType::try_from`].
    Error = 0x8f,
}

/// Represents the type of an error a HID++1.0 device returns as part of a
/// message with the [`MessageType::Error`] type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ErrorType {
    /// No error.
    Success = 0x00,

    /// The sub ID of a sent message is invalid.
    InvalidSubId = 0x01,

    /// The address included in a sent message is invalid.
    InvalidAddress = 0x02,

    /// The value included in a sent message is invalid.
    InvalidValue = 0x03,

    /// A connection request failed on the receiver's side.
    ConnectFail = 0x04,

    /// The receiver indicates that too many devices are connected to it.
    TooManyDevices = 0x05,

    /// The receiver indicates that something already exists. This error is not
    /// further documented, please let me know what it means.
    AlreadyExists = 0x06,

    /// The receiver is currently handling a downstream (to device) message and
    /// cannot process a second one.
    Busy = 0x07,

    /// Trying to send a message to a device (device index) where there is no
    /// device paired.
    UnknownDevice = 0x08,

    /// This error is returned by the receiver when a HID++ command has been
    /// sent to a device that is in disconnected mode. When a device is in
    /// disconnected mode it cannot receive commands from the host until it
    /// reconnects. A device reconnects when the user interacts with it. In most
    /// cases, a device disconnects after several minutes of inactivity.
    ResourceError = 0x09,

    /// A sent request is not available in the current context.
    RequestUnavailable = 0x0a,

    /// A request parameter has an unsupported value.
    InvalidParamValue = 0x0b,

    /// The PIN code of a device was wrong.
    WrongPinCode = 0x0c,
}

/// Represents an error that may occur when accessing registers using HID++1.0.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Hidpp10Error {
    /// Indicates that an error occurred while communicating across the HID++
    /// channel.
    #[error("the HID++ channel returned an error")]
    Channel(#[from] ChannelError),

    /// Indicates that a register access failed.
    #[error("a HID++1.0 register access resulted in an error")]
    RegisterAccess(ErrorType),

    /// Indicates that a received response is not fully supported.
    #[error("the received response from the device is (partly) unsupported")]
    UnsupportedResponse,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::join;

    use super::MessageType;
    use crate::channel::{
        HidppMessage,
        tests::{MockRawHidChannel, channel_with_reader},
    };

    /// A long RAP reply from the receiver for register `address` whose first
    /// data byte is `first` — the sub-register echo for `0xB5` reads.
    fn long_register_reply(address: u8, first: u8) -> HidppMessage {
        let mut data = [0u8; 19];
        data[0] = 0xff;
        data[1] = MessageType::GetLongRegister.into();
        data[2] = address;
        data[3] = first;
        data[4] = 0xaa;
        HidppMessage::Long(data)
    }

    #[test]
    fn sub_register_read_ignores_another_sub_registers_reply() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            // Arrives on the request's write: slot 3's pairing information,
            // as a second reader of the same node produces while this read is
            // pending. Matching on the header alone would accept it.
            handle.queue_response(long_register_reply(0xb5, 0x53));

            let read = channel.read_long_sub_register(0xff, 0xb5, 0x52, [0, 0]);
            let inject = handle.send_incoming(long_register_reply(0xb5, 0x52));
            let (result, ()) = join!(read, inject);

            let data = result.expect("the reply echoing the sub-register answers the read");
            assert_eq!(data[0], 0x52, "slot 2's read must not take slot 3's reply");
        });
    }

    #[test]
    fn plain_register_read_matches_on_the_header_alone() {
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let channel = Arc::new(channel_with_reader(raw).await);
            // Registers without a sub-register carry data in the first byte, so
            // the plain read must keep accepting whatever that byte holds.
            handle.queue_response(long_register_reply(0x02, 0x00));

            let data = channel
                .read_long_register(0xff, 0x02, [0, 0, 0])
                .await
                .expect("a header match answers a plain register read");
            assert_eq!(data[0], 0x00);
        });
    }
}
