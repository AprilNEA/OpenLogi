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
use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};

use super::{RECEIVER_DEVICE_INDEX, ReceiverError};
use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    protocol::v10::{self, Hidpp10Error},
};

/// All USB vendor & product ID pairs that are known to identify Bolt receivers.
pub const VPID_PAIRS: &[(u16, u16)] = &[(0x046d, 0xc548)];

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

                if header.device_index != RECEIVER_DEVICE_INDEX && header.sub_id != 0x41 {
                    return;
                }

                match header.sub_id {
                    // Device connection
                    0x41 => {
                        // Kind is identity-only; an unrecognised nibble folds
                        // to `Unknown` instead of dropping the event.
                        emitter.emit(Event::DeviceConnection(DeviceConnection {
                            index: header.device_index,
                            kind: DeviceKind::from(payload[1] & 0x0f),
                            encrypted: payload[1] & (1 << 5) != 0,
                            online: payload[1] & (1 << 6) == 0,
                            wpid: u16::from_le_bytes(payload[2..=3].try_into().unwrap()),
                        }));
                    }
                    // Device discovery
                    0x4f => {
                        match payload[2] {
                            // Device data
                            0 => {
                                emitter.emit(Event::DeviceDiscoveryDeviceDetails {
                                    counter: payload[0] as u16 + payload[1] as u16 * 256,
                                    kind: DeviceKind::from(payload[4] & 0x0f),
                                    wpid: u16::from_le_bytes(payload[5..=6].try_into().unwrap()),
                                    address: payload[7..=12].try_into().unwrap(),
                                    authentication: payload[15],
                                });
                            }
                            // Device name
                            1 => {
                                let Some((counter, name)) = parse_discovery_name(&payload) else {
                                    return;
                                };

                                emitter.emit(Event::DeviceDiscoveryDeviceName {
                                    counter,
                                    name: name.to_string(),
                                });
                            }
                            _ => (),
                        }
                    }
                    // Device discovery status
                    0x53 => {
                        emitter.emit(Event::DeviceDiscoveryStatus {
                            discovery_enabled: payload[0] == 0x00,
                        });
                    }
                    // Pairing status
                    0x54 => {
                        // payload[0] contains some kind of information about the status. I don't
                        // know how to map that though.

                        // An unrecognised error code still means "pairing
                        // failed" — dropping it here would turn the failure
                        // into a session timeout. Carry the raw code instead.
                        let error = (payload[1] != 0x00).then(|| PairingError::from(payload[1]));

                        emitter.emit(Event::PairingStatus {
                            device_address: payload[2..=7].try_into().unwrap(),
                            pairing_error: error,
                            slot: if payload[8] == 0x00 {
                                None
                            } else {
                                Some(payload[8])
                            },
                        });
                    }
                    // Passkey request
                    0x4d => {
                        // 6 bytes, NUL-padded when the passkey is shorter.
                        let digits = &payload[1..=6];
                        let len = digits.iter().position(|&b| b == 0).unwrap_or(digits.len());
                        let Ok(passkey) = str::from_utf8(&digits[..len]) else {
                            return;
                        };

                        emitter.emit(Event::PairingPasskeyRequest {
                            device_address: payload[7..=12].try_into().unwrap(),
                            passkey: passkey.to_string(),
                        });
                    }
                    // Passkey pressed
                    0x4e => {
                        emitter.emit(Event::PairingPasskeyPressed {
                            device_address: payload[1..=6].try_into().unwrap(),
                            press_type: PairingPasskeyPressType::from(payload[0]),
                        });
                    }
                    _ => (),
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
                [0, if state.wireless_notifications { 1 } else { 0 }, 0],
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
            wpid: u16::from_le_bytes(response[2..=3].try_into().unwrap()),
            // Kind is identity-only: an unrecognised nibble folds to
            // `Unknown` instead of failing the whole pairing-info read.
            kind: DeviceKind::from(response[1] & 0x0f),
            encrypted: response[1] & (1 << 5) != 0,
            online: response[1] & (1 << 6) == 0,
            unit_id: response[4..=7].try_into().unwrap(),
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

/// Parse a device-discovery name notification (sub-id `0x4f`, kind `1`).
///
/// `payload[3]` is the device-reported name length. The byte comes straight
/// off the radio, so it must never index past the report: a length that does
/// not fit the packet (or non-UTF-8 bytes) drops the event instead of
/// panicking the listener.
fn parse_discovery_name(payload: &[u8; 17]) -> Option<(u16, &str)> {
    let len = usize::from(payload[3]);
    let end = 4usize.checked_add(len)?;
    let name = str::from_utf8(payload.get(4..end)?).ok()?;
    Some((payload[0] as u16 + payload[1] as u16 * 256, name))
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

/// Represents the kind of a device paired to a Bolt receiver.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum DeviceKind {
    /// Unknown device kind — also the fold target for values this crate
    /// does not model (kind is identity-only and must never drop an event).
    #[num_enum(default)]
    Unknown = 0x00,
    /// Keyboard device.
    Keyboard = 0x01,
    /// Mouse device.
    Mouse = 0x02,
    /// Numeric keypad device.
    Numpad = 0x03,
    /// Presenter device.
    Presenter = 0x04,
    /// Remote-control device.
    Remote = 0x07,
    /// Trackball device.
    Trackball = 0x08,
    /// Touchpad device.
    Touchpad = 0x09,
    /// Tablet device.
    Tablet = 0x0a,
    /// Gamepad device.
    Gamepad = 0x0b,
    /// Joystick device.
    Joystick = 0x0c,
    /// Headset device.
    Headset = 0x0d,
}

/// Represents an event emitted by the receiver.
///
/// You can listen to these events using [`Receiver::listen`]. Only enabled
/// notifications as indicated by [`Receiver::get_notification_state`] are
/// emitted.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Event {
    /// Is emitted whenever a device connects to or disconnects from the
    /// receiver, but only if [`NotificationState::wireless_notifications`] is
    /// enabled.
    ///
    /// Can be triggered for all paired devices using
    /// [`Receiver::trigger_device_arrival`] to allow easy device enumeration.
    ///
    /// [`Receiver::collect_paired_devices`] implements a simple mechanism to
    /// collect all paired devices.
    DeviceConnection(DeviceConnection),

    /// Is emitted whenever the device discovery status changes.
    DeviceDiscoveryStatus {
        /// Whether discovery mode is enabled.
        discovery_enabled: bool,
    },

    /// Is emitted many times for every device discovered using
    /// [`Receiver::discover_devices`].
    ///
    /// This event contains device details, including its address required to
    /// start pairing. The [`Event::DeviceDiscoveryDeviceName`] event will also
    /// be emitted and contains the device name.
    DeviceDiscoveryDeviceDetails {
        /// The incrementing event counter. This can be used to map
        /// [`Event::DeviceDiscoveryDeviceDetails`] and
        /// [`Event::DeviceDiscoveryDeviceName`] events.
        counter: u16,

        /// Device kind reported by discovery.
        kind: DeviceKind,
        /// Wireless product ID of the discovered device.
        wpid: u16,

        /// The address of the device required to pair it using
        /// [`Receiver::pair_device`].
        ///
        /// This can also be used as the unique device identifier when
        /// collecting discovered devices.
        address: [u8; 6],

        /// The authentication type(s) the device supports. Unfortunately, there
        /// is not much information about this value and whether it is a
        /// single value or a bitfield.
        authentication: u8,
    },

    /// Is emitted many times for every device discovered using
    /// [`Receiver::discover_devices`].
    ///
    /// This event only contains the device name. Device details will be
    /// provided using the [`Event::DeviceDiscoveryDeviceDetails`] event.
    DeviceDiscoveryDeviceName {
        /// The incrementing event counter. This can be used to map
        /// [`Event::DeviceDiscoveryDeviceDetails`] and
        /// [`Event::DeviceDiscoveryDeviceName`] events.
        counter: u16,

        /// Discovered device name.
        name: String,
    },

    /// Is emitted whenever the status of a pairing process changes.
    PairingStatus {
        /// BTLE address of the device being paired.
        device_address: [u8; 6],
        /// Optional pairing error reported by the receiver.
        pairing_error: Option<PairingError>,

        /// The receiver slot the newly paired device was paired to. This can be
        /// used as the device index for subsequent operations.
        slot: Option<u8>,
    },

    /// Is emitted once the receiver requests a passkey to be entered on a
    /// device that should be paired to it.
    PairingPasskeyRequest {
        /// BTLE address of the device being paired.
        device_address: [u8; 6],

        /// The passkey the user has to enter in order to pair the device.
        ///
        /// Depending on the device and authentication type, this value has
        /// different implications.
        ///
        /// For mice, this value will be a valid 6-digit number. After parsing
        /// this into an integer, the (least significant) bits represent
        /// the sequence of mouse presses (`0` = left, `1` = right) the
        /// user has to perform, with an additional press of both mouse
        /// buttons simultaneously.
        ///
        /// The amount of bits significant to this equals to the `entropy`
        /// passed to [`Receiver::pair_device`].
        passkey: String,
    },

    /// Is emitted for every keypress a user performs while entering a pairing
    /// passkey.
    PairingPasskeyPressed {
        /// BTLE address of the device being paired.
        device_address: [u8; 6],

        /// The type of the keypress the user performed.
        ///
        /// Every passkey sequence starts with an event where this value is set
        /// to [`PairingPasskeyPressType::Initialization`]. Each time the user
        /// presses a key, an event with a press type of
        /// [`PairingPasskeyPressType::Keypress`] is emitted. Once the user
        /// submits their passkey, this value will be
        /// [`PairingPasskeyPressType::Submit`].
        press_type: PairingPasskeyPressType,
    },
}

/// Represents a device connected to a Bolt receiver.
///
/// This information is emitted by the [`Event::DeviceConnection`] event and can
/// be conveniently collected using [`Receiver::collect_paired_devices`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DeviceConnection {
    /// Slot index (1-based) of the device.
    pub index: u8,
    /// Device kind reported by the receiver.
    pub kind: DeviceKind,
    /// Whether the link is encrypted.
    pub encrypted: bool,
    /// Whether the device is currently online.
    pub online: bool,
    /// Wireless product ID of the device.
    pub wpid: u16,
}

/// Represents an error during device pairing.
///
/// This is reported by the [`Event::PairingStatus`] event.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, FromPrimitive, IntoPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum PairingError {
    /// Device timed out during pairing.
    DeviceTimeout = 0x01,
    /// Pairing failed.
    Failed = 0x02,
    /// An error code this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Represents the type of a single passkey press.
///
/// This is reported by the [`Event::PairingPasskeyPressed`] event, which also
/// includes some further information about the context of these values.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, FromPrimitive, IntoPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum PairingPasskeyPressType {
    /// Passkey entry has started.
    Initialization = 0x00,
    /// A passkey keypress was entered.
    Keypress = 0x01,
    /// Passkey entry was submitted.
    Submit = 0x04,
    /// A press type this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_name_with_oversized_length_is_dropped() {
        let mut payload = [0u8; 17];
        payload[3] = 200;

        assert_eq!(parse_discovery_name(&payload), None);
    }

    #[test]
    fn discovery_name_within_bounds_parses() {
        let mut payload = [0u8; 17];
        payload[0] = 7;
        payload[3] = 4;
        payload[4..8].copy_from_slice(b"Casa");

        assert_eq!(parse_discovery_name(&payload), Some((7, "Casa")));
    }

    #[test]
    fn discovery_name_rejects_invalid_utf8() {
        let mut payload = [0u8; 17];
        payload[3] = 2;
        payload[4] = 0xff;
        payload[5] = 0xfe;

        assert_eq!(parse_discovery_name(&payload), None);
    }

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
