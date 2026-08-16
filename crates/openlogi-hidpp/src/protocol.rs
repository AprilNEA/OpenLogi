//! Implements the protocol-specific parts of HID++.

use std::fmt::Debug;

use crate::{
    channel::{ChannelError, HidppChannel},
    nibble::{self, U4},
};

pub mod v10;
pub mod v20;

/// Represents the protocol version a device supports.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ProtocolVersion {
    /// The older HID++1.0 protocol. Mostly used for receivers.
    V10,

    /// All newer protocols starting from HID+2.0.
    ///
    /// Traditionally, the version was split into a major and a minor version,
    /// defining the concrete protocol version. These two values were later
    /// redefined to serve the purpose of indicating which host software to
    /// target.
    V20 {
        /// The protocol number is a field that hints the host software if it
        /// should support the device.
        ///
        /// `protocol_num = 2` : Intended target SW is Logitech SetPoint\
        /// `protocol_num = 3` : Intended OEM SW described in `target_sw` field\
        /// `protocol_num = 4` : Intended target SW described in `target_sw`
        /// field
        protocol_num: u8,

        /// When `protocol_num >= 3` this field further hints at which software
        /// should support the device. Otherwise the value is zero.
        ///
        /// See <https://drive.google.com/file/d/1ULmw9uJL8b8iwwUo5xjSS9F5Zvno-86y/view>
        /// for more information.
        target_sw: u8,
    },
}

/// Tries to determine the protocol version of a specific device.
///
/// Returns `Ok(None)` if no device was found for the given device index.
pub async fn determine_version(
    chan: &HidppChannel,
    device_index: u8,
) -> Result<Option<ProtocolVersion>, ChannelError> {
    // To determine the protocol version, we send a HID++2.0 ping message
    // feature with index 0x00, function 0x01).
    // Devices supporting protocol >=2.0 will respond with a defined response
    // including the particular protocol version.
    // Devices only supporting protocol 1.0 will respond with an error message
    // indicating 0x00 is no valid sub ID. We make use of this to pin them to
    // version 1.0.

    let sw_id = chan.get_sw_id();
    let msg = v20::Message::Short(
        v20::MessageHeader {
            device_index,
            feature_index: 0x00,
            function_id: U4::from_lo(0x1),
            software_id: sw_id,
        },
        [0x00, 0x00, 0x00],
    );

    let response = chan
        .send(msg.into(), move |resp| {
            // If we receive a valid HID++2.0 response, we'll use that.
            if v20::Message::from(*resp).header() == msg.header() {
                return true;
            }

            // We only care about HID++1.0 error messages, which are always short according
            // to the spec.
            if let v10::Message::Short(header, payload) = v10::Message::from(*resp)
                && header.device_index == device_index
                    && header.sub_id == v10::MessageType::Error.into()
                    // The feature index we sent would be interpreted as the sub ID by HID++1.0, which is included in the error message.
                    && payload[0] == 0x00
                    // The function & software IDs would be interpreted as the register address in HID++1.0.
                    && payload[1] == nibble::combine(msg.header().function_id, sw_id)
            {
                return true;
            }

            false
        })
        .await?;

    let v20_msg = v20::Message::from(response);
    if v20_msg.header() == msg.header() {
        let payload = v20_msg.extend_payload();
        return Ok(Some(ProtocolVersion::V20 {
            protocol_num: payload[0],
            target_sw: payload[1],
        }));
    }

    let v10::Message::Short(_, payload) = v10::Message::from(response) else {
        return Ok(None);
    };

    if payload[2] == v10::ErrorType::InvalidSubId.into() {
        Ok(Some(ProtocolVersion::V10))
    } else {
        Ok(None)
    }
}
