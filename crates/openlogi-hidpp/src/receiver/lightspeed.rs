//! Implements Logitech LIGHTSPEED gaming receivers.
//!
//! LIGHTSPEED receivers use the same HID++ 1.0 receiver registers as the
//! Unifying family for inventory and address paired HID++ 2.0 devices by slot.
//! They are kept as a separate receiver family because their USB product IDs,
//! pairing behavior, and supported device population are distinct.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    protocol::v10::{self, Hidpp10Error},
    receiver::{RECEIVER_DEVICE_INDEX, ReceiverError},
};

/// All USB vendor/product pairs known to identify LIGHTSPEED receivers.
pub const VPID_PAIRS: &[(u16, u16)] = &[
    (0x046d, 0xc539),
    (0x046d, 0xc53a),
    (0x046d, 0xc53d),
    (0x046d, 0xc53f),
    (0x046d, 0xc541),
    (0x046d, 0xc545),
    (0x046d, 0xc547),
    (0x046d, 0xc54d),
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
enum Register {
    Connections = 0x02,
    ReceiverInfo = 0xb5,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
enum InfoSubRegister {
    ReceiverInfo = 0x03,
    DevicePairingInformation = 0x20,
    ExtendedPairingInformation = 0x30,
    DeviceCodename = 0x40,
}

/// A Logitech LIGHTSPEED receiver.
#[derive(Clone)]
pub struct Receiver {
    chan: Arc<HidppChannel>,
    emitter: Arc<EventEmitter<Event>>,
    _listener: Arc<MessageListenerGuard>,
}

impl Receiver {
    /// Creates a LIGHTSPEED receiver for a channel with a known VID/PID.
    pub fn new(chan: Arc<HidppChannel>) -> Result<Self, ReceiverError> {
        if !VPID_PAIRS.contains(&(chan.vendor_id, chan.product_id)) {
            return Err(ReceiverError::UnknownReceiver);
        }

        let emitter = Arc::new(EventEmitter::new());
        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);
            move |raw, matched| {
                if matched {
                    return;
                }
                if let Some(connection) = parse_connection_notification(v10::Message::from(raw)) {
                    emitter.emit(Event::DeviceConnection(connection));
                }
            }
        });

        Ok(Self {
            chan,
            emitter,
            _listener: Arc::new(listener),
        })
    }

    /// Creates a listener for receiver events.
    pub fn listen(&self) -> async_channel::Receiver<Event> {
        self.emitter.create_receiver()
    }

    /// Returns the number of persistent pairings on the receiver.
    pub async fn count_pairings(&self) -> Result<u8, ReceiverError> {
        let response = self
            .chan
            .read_register(RECEIVER_DEVICE_INDEX, Register::Connections.into(), [0; 3])
            .await?;
        Ok(response[1])
    }

    /// Requests a `0x41` notification from every connected paired device.
    pub async fn trigger_device_arrival(&self) -> Result<(), ReceiverError> {
        self.chan
            .write_register(
                RECEIVER_DEVICE_INDEX,
                Register::Connections.into(),
                [0x02, 0x00, 0x00],
            )
            .await?;
        Ok(())
    }

    /// Reads receiver serial identity and pairing capacity.
    pub async fn get_receiver_info(&self) -> Result<ReceiverInfo, ReceiverError> {
        let response = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [InfoSubRegister::ReceiverInfo.into(), 0, 0],
            )
            .await?;
        Ok(ReceiverInfo {
            serial_number: hex::encode_upper(&response[1..=4]),
            pairing_slots: response[6],
        })
    }

    /// Reads persistent pairing metadata for a 1-based receiver slot.
    pub async fn get_device_pairing_information(
        &self,
        device_index: u8,
    ) -> Result<DevicePairingInformation, ReceiverError> {
        let pairing = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [
                    u8::from(InfoSubRegister::DevicePairingInformation)
                        .saturating_add(device_index.saturating_sub(1)),
                    0,
                    0,
                ],
            )
            .await?;
        let extended = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [
                    u8::from(InfoSubRegister::ExtendedPairingInformation)
                        .saturating_add(device_index.saturating_sub(1)),
                    0,
                    0,
                ],
            )
            .await
            .ok();
        decode_pairing_information(&pairing, extended.as_ref()).map_err(ReceiverError::from)
    }

    /// Reads the receiver-provided codename for a paired device.
    pub async fn get_device_codename(&self, device_index: u8) -> Result<String, ReceiverError> {
        let response = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [
                    u8::from(InfoSubRegister::DeviceCodename)
                        .saturating_add(device_index.saturating_sub(1)),
                    0,
                    0,
                ],
            )
            .await?;
        decode_codename(&response)
            .map(str::to_string)
            .ok_or_else(|| ReceiverError::from(Hidpp10Error::UnsupportedResponse))
    }

    /// Returns the stable receiver serial number.
    pub async fn get_unique_id(&self) -> Result<String, ReceiverError> {
        self.get_receiver_info()
            .await
            .map(|info| info.serial_number)
    }
}

fn parse_connection_notification(message: v10::Message) -> Option<DeviceConnection> {
    let header = message.header();
    if header.sub_id != 0x41 || header.device_index == RECEIVER_DEVICE_INDEX {
        return None;
    }
    let payload = message.extend_payload();
    Some(DeviceConnection {
        index: header.device_index,
        kind: DeviceKind::try_from(payload[1] & 0x0f).ok()?,
        encrypted: payload[1] & (1 << 5) != 0 || payload[0] == 0x10,
        online: payload[1] & (1 << 6) == 0,
        wpid: u16::from_le_bytes([payload[2], payload[3]]),
    })
}

fn decode_pairing_information(
    response: &[u8; 16],
    extended: Option<&[u8; 16]>,
) -> Result<DevicePairingInformation, Hidpp10Error> {
    Ok(DevicePairingInformation {
        wpid: u16::from_be_bytes([response[3], response[4]]),
        kind: DeviceKind::try_from(response[7] & 0x0f)
            .map_err(|_| Hidpp10Error::UnsupportedResponse)?,
        encrypted: false,
        // Pairing registers are persistent and carry no live connection bit;
        // a `0x41` notification overrides this when the device is awake.
        online: false,
        unit_id: extended.map_or([0; 4], |info| info[1..=4].try_into().unwrap()),
    })
}

fn decode_codename(response: &[u8; 16]) -> Option<&str> {
    let len = usize::from(response[1]).min(14);
    core::str::from_utf8(&response[2..2 + len]).ok()
}

/// General LIGHTSPEED receiver information.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ReceiverInfo {
    /// Stable receiver serial number.
    pub serial_number: String,
    /// Number of pairing slots supported by the receiver.
    pub pairing_slots: u8,
}

/// Persistent metadata for one paired device.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DevicePairingInformation {
    /// Wireless product ID.
    pub wpid: u16,
    /// Receiver-reported device type.
    pub kind: DeviceKind,
    /// Whether the wireless link is encrypted.
    pub encrypted: bool,
    /// Whether the paired device is currently connected.
    pub online: bool,
    /// Device unit identifier stored by the receiver.
    pub unit_id: [u8; 4],
}

/// Device-kind encoding used by LIGHTSPEED pairing registers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DeviceKind {
    /// Unidentified device.
    Unknown = 0x00,
    /// Keyboard.
    Keyboard = 0x01,
    /// Mouse.
    Mouse = 0x02,
    /// Numeric keypad.
    Numpad = 0x03,
    /// Presenter.
    Presenter = 0x04,
    /// Remote control.
    Remote = 0x05,
    /// Trackball.
    Trackball = 0x06,
    /// Touchpad.
    Touchpad = 0x07,
}

/// A parsed `0x41` device connection notification.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceConnection {
    /// Receiver slot index.
    pub index: u8,
    /// Receiver-reported device type.
    pub kind: DeviceKind,
    /// Whether the wireless link is encrypted.
    pub encrypted: bool,
    /// Whether the device is online.
    pub online: bool,
    /// Wireless product ID.
    pub wpid: u16,
}

/// Events emitted by a LIGHTSPEED receiver.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Event {
    /// A paired device connected, disconnected, or answered an arrival query.
    DeviceConnection(DeviceConnection),
}

#[cfg(test)]
mod tests {
    use crate::protocol::v10::{Message, MessageHeader};

    use super::{
        DeviceKind, VPID_PAIRS, decode_pairing_information, parse_connection_notification,
    };

    #[test]
    fn known_pid_family_is_complete() {
        let pids: Vec<u16> = VPID_PAIRS.iter().map(|(_, pid)| *pid).collect();
        assert_eq!(
            pids,
            [
                0xc539, 0xc53a, 0xc53d, 0xc53f, 0xc541, 0xc545, 0xc547, 0xc54d
            ]
        );
    }

    #[test]
    fn parses_g305_connection_notification() {
        let message = Message::Short(
            MessageHeader {
                device_index: 1,
                sub_id: 0x41,
            },
            [0, 0x22, 0x74, 0x40],
        );
        let connection = parse_connection_notification(message).unwrap();
        assert_eq!(connection.index, 1);
        assert_eq!(connection.kind, DeviceKind::Mouse);
        assert!(connection.encrypted && connection.online);
        assert_eq!(connection.wpid, 0x4074);
    }

    #[test]
    fn decodes_pairing_register_metadata() {
        let mut response = [0u8; 16];
        response[3..=4].copy_from_slice(&0x4074u16.to_be_bytes());
        response[7] = 0x02;
        let mut extended = [0u8; 16];
        extended[1..=4].copy_from_slice(&[1, 2, 3, 4]);
        let pairing = decode_pairing_information(&response, Some(&extended)).unwrap();
        assert_eq!(pairing.wpid, 0x4074);
        assert_eq!(pairing.kind, DeviceKind::Mouse);
        assert!(!pairing.encrypted && !pairing.online);
        assert_eq!(pairing.unit_id, [1, 2, 3, 4]);
    }
}
