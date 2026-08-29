//! Implements the Logi Bolt receiver.
//!
//! Bolt can be seen as a successor to the Unifying receiver. Both of them
//! support up to 6 paired devices, but Bolt uses BTLE technology and introduces
//! so-called passkeys for authenticating devices before pairing them.
//!
//! There is little to no public documentation about what registers Bolt
//! supports (and they seem to differ quite substantially from registers
//! supported by Unifying and other receivers), so this implementation is based
//! largely on information gathered by looking at other codebases (primarily
//! Solaar) and searching registers by fuzzing them.

use std::sync::Arc;

use derive_builder::Builder;
use futures::{FutureExt, pin_mut, select};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_device_registry::receiver::{ReceiverProtocol, find_receiver};

use super::{RECEIVER_DEVICE_INDEX, ReceiverError};
use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    emitter::EventEmitter,
    protocol::v10::{self, Hidpp10Error},
};

mod event;

pub use event::{
    DeviceConnection, DeviceKind, Event, PairingPasskeyPressType, decode as decode_notification,
};

/// All known registers of the Bolt receiver.
///
/// In most cases you should not need to access these manually, as [`Receiver`]
/// implements many features.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Register {
    /// Allows control over what notifications the receiver sends.
    Notifications = 0x00,

    /// Provides the amount of currently paired devices.
    ///
    /// This is exposed by [`Receiver::count_pairings`].
    Connections = 0x02,

    /// Provides information about the receiver and paired devices.
    ///
    /// It uses sub-registers, as defined in [`InfoSubRegister`], to
    /// differentiate between different kinds of information.
    ReceiverInfo = 0xb5,

    /// Provides support for discovering devices that are ready to pair.
    ///
    /// Use [`Receiver::discover_devices`] and
    /// [`Receiver::cancel_device_discovery`] to control device discovery.
    DeviceDiscovery = 0xc0,

    /// Provides pairing and unpairing support.
    ///
    /// Use [`Receiver::pair_device`] and [`Receiver::unpair_device`] for
    /// pairing and unpairing.
    Pairing = 0xc1,

    /// Exposes the unique ID of the receiver. This seems to differ from the
    /// serial number.
    ///
    /// Use [`Receiver::get_unique_id`] to query this value.
    UniqueId = 0xfb,
}

/// All known sub-registers of the [`Register::ReceiverInfo`] register.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum InfoSubRegister {
    /// Provides information about a specific paired device. The device index (4
    /// bits) has to be added to the register address.
    ///
    /// Exposed by [`Receiver::get_device_pairing_information`].
    DevicePairingInformation = 0x50, // 0x5N with N = device index

    /// Provides the name of a paired device. The device index (4
    /// bits) has to be added to the register address.
    ///
    /// Exposed by [`Receiver::get_device_codename`].
    DeviceCodename = 0x60, // 0x6N with N = device index
}

/// Implements the Bolt receiver.
#[derive(Clone)]
pub struct Receiver {
    chan: Arc<HidppChannel>,
    emitter: Arc<EventEmitter<Event>>,
    _listener: Arc<MessageListenerGuard>,
}

impl Receiver {
    /// Tries to initialize a new [`Receiver`] from a raw HID++ channel.
    ///
    /// If no receiver could be found, or if the vendor and product IDs don't
    /// match the ones of any known Bolt receiver, this function will return
    /// [`ReceiverError::UnknownReceiver`].
    pub fn new(chan: Arc<HidppChannel>) -> Result<Self, ReceiverError> {
        if find_receiver(chan.vendor_id, chan.product_id)
            .is_none_or(|receiver| receiver.protocol != ReceiverProtocol::Bolt)
        {
            return Err(ReceiverError::UnknownReceiver);
        }

        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                // A report already matched to an outgoing request is a
                // response, not a notification.
                if matched {
                    return;
                }

                if let Some(event) = event::decode(&v10::Message::from(raw)) {
                    emitter.emit(event);
                }
            }
        });

        Ok(Receiver {
            _listener: Arc::new(listener),
            chan,
            emitter,
        })
    }

    /// Creates a new listener for receiving receiver events.
    #[must_use]
    pub fn listen(&self) -> async_channel::Receiver<Event> {
        self.emitter.create_receiver()
    }

    /// Queries the current information about what notifications are enabled.
    pub async fn get_notification_state(&self) -> Result<NotificationState, ReceiverError> {
        let response = self
            .chan
            .read_register(
                RECEIVER_DEVICE_INDEX,
                Register::Notifications.into(),
                [0u8; 3],
            )
            .await?;

        Ok(NotificationState {
            wireless_notifications: (response[1] & 1) != 0,
        })
    }

    /// Configures what notifications are enabled and thus reported by the
    /// receiver.
    pub async fn set_notification_state(
        &self,
        state: NotificationState,
    ) -> Result<(), ReceiverError> {
        self.chan
            .write_register(
                RECEIVER_DEVICE_INDEX,
                Register::Notifications.into(),
                [0, u8::from(state.wireless_notifications), 0],
            )
            .await?;

        Ok(())
    }

    /// Counts the amount of devices currently paired to this receiver. The
    /// devices don't have to be online to be included here as pairings are
    /// persistent.
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

    /// Triggers device arrival notifications for all devices currently
    /// connected to the receiver. This is useful for device enumeration.
    ///
    /// Check [`Self::get_notification_state`] first to make sure that
    /// [`NotificationState::wireless_notifications`] is enabled.
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

    /// Collects information about all paired devices by calling
    /// [`Self::trigger_device_arrival`] and collecting incoming
    /// [`Event::DeviceConnection`] events.
    ///
    /// Check [`Self::get_notification_state`] first to make sure that
    /// [`NotificationState::wireless_notifications`] is enabled.
    pub async fn collect_paired_devices(&self) -> Result<Vec<DeviceConnection>, ReceiverError> {
        // The idea here is that, when triggering fake device arrival notifications, the
        // receiver will send the register write confirmation message only AFTER sending
        // all arrival notifications.
        // So we will trigger device arrival notifications and continue collecting those
        // until the original future has completed.

        let mut devices = vec![];

        let rx = self.listen();
        let fin = self.trigger_device_arrival().fuse();
        pin_mut!(fin);

        loop {
            select! {
                _ = fin => break,
                res = rx.recv().fuse() => {
                    let Ok(Event::DeviceConnection(connection)) = res else {
                        continue;
                    };

                    devices.push(connection);
                }
            }
        }

        Ok(devices)
    }

    /// Retrieves the unique ID of the receiver. This is not the same as the
    /// serial number.
    pub async fn get_unique_id(&self) -> Result<String, ReceiverError> {
        let response = self
            .chan
            .read_long_register(RECEIVER_DEVICE_INDEX, Register::UniqueId.into(), [0u8; 3])
            .await?;

        // When decoding the last 8 bytes of the response to their ASCII representation
        // we seem to get a valid hex string representing 4 bytes of data.
        // Interpreting this hex string as little endian we seem to get the same decimal
        // value the Options+ software calls `udid` (unique device identifier?). I am
        // not sure what this is about and it may be a (major) coincidence that these
        // values match for my receiver, but it could be worth keeping this in mind.

        // I have no clue how to retrieve the serial number of the receiver.

        Ok(str::from_utf8(&response)
            .map_err(|_| Hidpp10Error::UnsupportedResponse)?
            .to_string())
    }

    /// Provides the pairing information of a specific paired device by its
    /// index.
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
                    u8::from(InfoSubRegister::DevicePairingInformation) + (device_index & 0x0f),
                    0x00,
                    0x00,
                ],
            )
            .await?;

        Ok(DevicePairingInformation {
            wpid: u16::from_le_bytes([response[2], response[3]]),
            // Kind is identity-only: an unrecognised nibble folds to
            // `Unknown` instead of failing the whole pairing-info read.
            kind: DeviceKind::from(response[1] & 0x0f),
            encrypted: response[1] & (1 << 5) != 0,
            online: response[1] & (1 << 6) == 0,
            unit_id: [response[4], response[5], response[6], response[7]],
        })
    }

    /// Provides the codename of a specific paired device by its index.
    pub async fn get_device_codename(&self, device_index: u8) -> Result<String, ReceiverError> {
        // For device names longer than 13 characters this may need to be called
        // multiple times with different parameters. I don't have a device with
        // such a name to be able to test this.

        let response = self
            .chan
            .read_long_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                [
                    u8::from(InfoSubRegister::DeviceCodename) + (device_index & 0x0f),
                    0x01,
                    0x00,
                ],
            )
            .await?;

        Ok(parse_codename(&response)
            .ok_or(Hidpp10Error::UnsupportedResponse)?
            .to_string())
    }

    /// Unpairs a device from the receiver by its index.
    pub async fn unpair_device(&self, device_index: u8) -> Result<(), ReceiverError> {
        let mut payload = [0u8; 16];
        payload[0] = 0x03;
        payload[1] = device_index;

        self.chan
            .write_long_register(RECEIVER_DEVICE_INDEX, Register::Pairing.into(), payload)
            .await?;

        Ok(())
    }

    /// Starts the pairing process for a new device.
    ///
    /// The required `address` and `authentication` values are usually
    /// discovered from the [`Event::DeviceDiscoveryDeviceDetails`] event which
    /// is emitted regularly when actively discovering available devices
    /// ([`Self::discover_devices`]).
    ///
    /// `entropy` specifies how complex the authentication passkey should be.
    /// For mice, this defines the amount of keypresses (left or right) the user
    /// has to perform. Not all values seem to be supported.
    pub async fn pair_device(
        &self,
        slot: u8,
        address: [u8; 6],
        authentication: u8,
        entropy: u8,
    ) -> Result<(), ReceiverError> {
        let mut payload = [0u8; 16];
        payload[0] = 0x01;
        payload[1] = slot;
        payload[2..=7].copy_from_slice(&address);
        payload[8] = authentication;
        payload[9] = entropy;

        self.chan
            .write_long_register(RECEIVER_DEVICE_INDEX, Register::Pairing.into(), payload)
            .await?;

        Ok(())
    }

    /// Starts device discovery for `timeout` seconds ([`None`] = default, seems
    /// to be 30s). The maximum supported value is 60s.
    ///
    /// While device discovery is enabled,
    /// [`Event::DeviceDiscoveryDeviceDetails`] and
    /// [`Event::DeviceDiscoveryDeviceName`] events are emitted for every
    /// discovered device.
    pub async fn discover_devices(&self, timeout: Option<u8>) -> Result<(), ReceiverError> {
        self.chan
            .write_register(
                RECEIVER_DEVICE_INDEX,
                Register::DeviceDiscovery.into(),
                [timeout.unwrap_or(0x00), 0x01, 0x00],
            )
            .await?;

        Ok(())
    }

    /// Cancels the device discovery process.
    pub async fn cancel_device_discovery(&self) -> Result<(), ReceiverError> {
        self.chan
            .write_register(
                RECEIVER_DEVICE_INDEX,
                Register::DeviceDiscovery.into(),
                [0x00, 0x02, 0x00],
            )
            .await?;

        Ok(())
    }
}

/// Extract the codename chunk from a `DeviceCodename` register read.
///
/// `response[2]` is the device-reported name length. A name longer than the
/// 13 bytes one response carries is clamped to the chunk present (fetching
/// the rest takes further reads with different parameters); a length byte
/// pointing past the response must not panic. `None` for non-UTF-8 bytes.
fn parse_codename(response: &[u8; 16]) -> Option<&str> {
    let end = 3usize.saturating_add(usize::from(response[2]));
    let raw = response.get(3..end.min(response.len()))?;
    str::from_utf8(raw).ok()
}

/// Indicates which notifications are enabled and thus sent by the receiver.
///
/// This information can be queried using [`Receiver::get_notification_state`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Builder)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct NotificationState {
    /// Whether the receiver sends device arrival/removal notifications.
    pub wireless_notifications: bool,
}

/// Represents information about a paired device.
///
/// This information can be queried using
/// [`Receiver::get_device_pairing_information`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DevicePairingInformation {
    /// Wireless product ID of the paired device.
    pub wpid: u16,
    /// Device kind reported by the receiver.
    pub kind: DeviceKind,
    /// Whether the link is encrypted.
    pub encrypted: bool,
    /// Whether the device is currently online.
    pub online: bool,
    /// Device unit ID.
    pub unit_id: [u8; 4],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codename_with_oversized_length_clamps_to_available_chunk() {
        let mut response = [0u8; 16];
        response[2] = 200;
        response[3..16].copy_from_slice(b"MX Anywhere 3");

        assert_eq!(parse_codename(&response), Some("MX Anywhere 3"));
    }

    #[test]
    fn codename_within_bounds_parses() {
        let mut response = [0u8; 16];
        response[2] = 5;
        response[3..8].copy_from_slice(b"Casa!");

        assert_eq!(parse_codename(&response), Some("Casa!"));
    }

    #[test]
    fn codename_rejects_invalid_utf8() {
        let mut response = [0u8; 16];
        response[2] = 2;
        response[3] = 0xff;
        response[4] = 0xfe;

        assert_eq!(parse_codename(&response), None);
    }
}
