//! Implements the Unifying Receiver.
//!
//! Unifying is a versatile receiver that can pair up to 6 devices using the
//! 2.4 GHz eQuad radio protocol. It uses HID++ 1.0 registers for receiver
//! control; paired devices speak HID++ 2.0 once addressed via their slot index.
//!
//! The register layout for device enumeration (`0xB5/0x5N`, `0xB5/0x6N`) is
//! identical to Bolt's. The device-kind encoding differs from Bolt at values 5+
//! (see [`DeviceKind`]).

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    protocol::v10::{self, Hidpp10Error},
    receiver::{RECEIVER_DEVICE_INDEX, ReceiverError},
};

/// All USB vendor & product ID pairs that are known to identify Unifying
/// receivers.
pub const VPID_PAIRS: &[(u16, u16)] = &[(0x046d, 0xc52b), (0x046d, 0xc532)];

/// All known registers of the Unifying receiver.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Register {
    /// Enables or disables wireless device-connection notifications; also used
    /// to read the pairing count and to trigger device-arrival events.
    Connections = 0x02,

    /// Provides information about the receiver and paired devices. It uses
    /// sub-registers, as defined in [`InfoSubRegister`], to differentiate
    /// between different kinds of information.
    ReceiverInfo = 0xb5,
}

/// Represents the known sub-registers of the [`Register::ReceiverInfo`]
/// register.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum InfoSubRegister {
    /// Provides general information about the receiver (serial number, pairing
    /// slot count).
    ReceiverInfo = 0x03,

    /// Provides information about a specific paired device. The device index
    /// (4 bits) must be added to this base address to form the actual
    /// sub-register: `0x50 | (device_index & 0x0f)`.
    DevicePairingInformation = 0x50,

    /// Provides the codename of a specific paired device. The device index (4
    /// bits) must be added: `0x60 | (device_index & 0x0f)`.
    DeviceCodename = 0x60,
}

/// Implements the Unifying wireless receiver.
#[derive(Clone)]
pub struct Receiver {
    chan: Arc<HidppChannel>,
    emitter: Arc<EventEmitter<Event>>,
    _listener: Arc<MessageListenerGuard>,
}

impl Receiver {
    /// Tries to initialize a new [`Receiver`] from a raw HID++ channel.
    ///
    /// Returns [`ReceiverError::UnknownReceiver`] when the channel's VID/PID
    /// doesn't match any known Unifying receiver.
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

                let parsed = v10::Message::from(raw);
                let header = parsed.header();
                let payload = parsed.extend_payload();

                // Device-connection notifications are directed at a specific slot
                // (header.device_index = slot) with sub_id 0x41.
                if header.sub_id != 0x41 {
                    return;
                }

                let Ok(kind) = DeviceKind::try_from(payload[1] & 0x0f) else {
                    return;
                };

                emitter.emit(Event::DeviceConnection(DeviceConnection {
                    index: header.device_index,
                    kind,
                    encrypted: payload[1] & (1 << 4) != 0,
                    online: payload[1] & (1 << 6) == 0,
                    wpid: u16::from_le_bytes(payload[2..=3].try_into().unwrap()),
                }));
            }
        });

        Ok(Receiver {
            _listener: Arc::new(listener),
            chan,
            emitter,
        })
    }

    /// Creates a new listener for receiving receiver events.
    pub fn listen(&self) -> async_channel::Receiver<Event> {
        self.emitter.create_receiver()
    }

    /// Counts the number of devices currently paired to this receiver.
    /// Offline (sleeping) devices are included since pairings are persistent.
    pub async fn count_pairings(&self) -> Result<u8, ReceiverError> {
        let response = self
            .chan
            .read_register(
                RECEIVER_DEVICE_INDEX,
                Register::Connections.into(),
                [0u8; 3],
            )
            .await?;

        Ok(response[1])
    }

    /// Triggers device-arrival notifications for all currently connected
    /// devices. Used to enumerate online devices at startup.
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

    /// Provides general information about the receiver (serial number and
    /// pairing slot count).
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

    /// Retrieves the pairing information for the device at `device_index`
    /// (1-based slot number).
    pub async fn get_device_pairing_information(
        &self,
        device_index: u8,
    ) -> Result<DevicePairingInformation, ReceiverError> {
        let response = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [
                    u8::from(InfoSubRegister::DevicePairingInformation) | (device_index & 0x0f),
                    0x00,
                    0x00,
                ],
            )
            .await?;

        Ok(DevicePairingInformation {
            wpid: u16::from_le_bytes(response[2..=3].try_into().unwrap()),
            kind: DeviceKind::try_from(response[1] & 0x0f)
                .map_err(|_| Hidpp10Error::UnsupportedResponse)?,
            encrypted: response[1] & (1 << 4) != 0,
            online: response[1] & (1 << 6) == 0,
            unit_id: response[4..=7].try_into().unwrap(),
        })
    }

    /// Provides the unique ID of the receiver (serial number).
    pub async fn get_unique_id(&self) -> Result<String, ReceiverError> {
        self.get_receiver_info().await.map(|i| i.serial_number)
    }
}

/// Represents some general information about a Unifying receiver.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ReceiverInfo {
    pub serial_number: String,
    pub pairing_slots: u8,
}

/// Represents information about a paired device as read from the pairing
/// register.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DevicePairingInformation {
    pub wpid: u16,
    pub kind: DeviceKind,
    pub encrypted: bool,
    pub online: bool,
    pub unit_id: [u8; 4],
}

/// Represents the kind of a device paired to a Unifying receiver.
///
/// The encoding matches Bolt for values 1–4; from 5 onwards Unifying uses a
/// shifted table (Remote=5, Trackball=6, Touchpad=7) while Bolt reserves those
/// values and places them at 7–9.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DeviceKind {
    Unknown = 0x00,
    Keyboard = 0x01,
    Mouse = 0x02,
    Numpad = 0x03,
    Presenter = 0x04,
    Remote = 0x05,
    Trackball = 0x06,
    Touchpad = 0x07,
}

/// Represents a device-connection event fired by the receiver when a paired
/// device comes online (or in response to [`Receiver::trigger_device_arrival`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceConnection {
    /// Slot index (1-based) of the device.
    pub index: u8,
    pub kind: DeviceKind,
    pub encrypted: bool,
    pub online: bool,
    /// Wireless product ID of the device.
    pub wpid: u16,
}

/// Represents an event emitted by the Unifying receiver.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Event {
    /// Fired whenever a paired device connects or reconnects, and for all
    /// online devices in response to [`Receiver::trigger_device_arrival`].
    DeviceConnection(DeviceConnection),
}
