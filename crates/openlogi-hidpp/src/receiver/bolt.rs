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
use std::time::Duration;

use futures::{FutureExt, pin_mut, select};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_device_registry::receiver::{ReceiverProtocol, find_receiver};

use super::{
    NotificationFlags, RECEIVER_DEVICE_INDEX, ReceiverError, read_notification_flags,
    write_notification_flags,
};
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

    /// Queries which notifications are enabled.
    pub async fn get_notification_flags(&self) -> Result<NotificationFlags, ReceiverError> {
        Ok(read_notification_flags(&self.chan).await?)
    }

    /// Configures which notifications are enabled. Edit the value from
    /// [`Self::get_notification_flags`]: the write replaces every bit,
    /// including ones other host software set.
    pub async fn set_notification_flags(
        &self,
        flags: NotificationFlags,
    ) -> Result<(), ReceiverError> {
        Ok(write_notification_flags(&self.chan, flags).await?)
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
    /// Check [`Self::get_notification_flags`] first to make sure that
    /// [`NotificationFlags::WIRELESS`] is enabled.
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
    /// Check [`Self::get_notification_flags`] first to make sure that
    /// [`NotificationFlags::WIRELESS`] is enabled.
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
            .read_long_sub_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                u8::from(InfoSubRegister::DevicePairingInformation) + (device_index & 0x0f),
                [0x00, 0x00],
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
    ///
    /// The first read (param `0x01`) carries the name's length and its first
    /// 13 bytes. A longer name is completed by a second read (param `0x02`),
    /// whose reply is `[sub_reg, 0x02]` followed by the next 14 name bytes
    /// with no repeated length — captured from a Bolt receiver with an
    /// MX Anywhere 3S paired (#1962). Only those two reads are confirmed, so a
    /// name past their 27 bytes stays truncated rather than guessing a third.
    /// If the second read fails, times out after a short wait, or does not
    /// echo its chunk, the first 13 bytes are returned, as before.
    pub async fn get_device_codename(&self, device_index: u8) -> Result<String, ReceiverError> {
        let sub_register = u8::from(InfoSubRegister::DeviceCodename) + (device_index & 0x0f);
        let first = self
            .chan
            .read_long_sub_register(
                RECEIVER_DEVICE_INDEX,
                Register::ReceiverInfo.into(),
                sub_register,
                [0x01, 0x00],
            )
            .await?;
        let second = if codename_continues(&first) {
            self.chan
                .read_long_sub_register_with_timeout(
                    RECEIVER_DEVICE_INDEX,
                    Register::ReceiverInfo.into(),
                    sub_register,
                    [0x02, 0x00],
                    CODENAME_CONTINUATION_TIMEOUT,
                )
                .await
                .ok()
        } else {
            None
        };

        Ok(assemble_codename(&first, second.as_ref()).ok_or(Hidpp10Error::UnsupportedResponse)?)
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

/// Name bytes the first `DeviceCodename` chunk carries, after
/// `[sub_reg, chunk, length]`.
const FIRST_CHUNK_NAME_BYTES: usize = 13;

/// Name bytes a second `DeviceCodename` chunk carries, after `[sub_reg, chunk]`.
const SECOND_CHUNK_NAME_BYTES: usize = 14;

/// How long the optional second `DeviceCodename` read may wait. The receiver
/// answers its own registers in milliseconds, and this read runs in the
/// sequential register pass before any slot's feature walk, inside the
/// receiver probe budget. A lost reply costs this wait plus the channel's
/// [`STALE_REPLY_GRACE`](crate::channel::STALE_REPLY_GRACE) on the next
/// register read, so it is kept short: the first 13 bytes are still a usable
/// name, and the five-second default would push a slot falling back on its
/// cached data past the receiver's budget.
const CODENAME_CONTINUATION_TIMEOUT: Duration = Duration::from_millis(250);

/// Whether the first `DeviceCodename` chunk reports a name longer than it holds.
fn codename_continues(first: &[u8; 16]) -> bool {
    usize::from(first[2]) > FIRST_CHUNK_NAME_BYTES
}

/// Join a `DeviceCodename` read into the name.
///
/// `first[2]` is the device-reported name length, and `first[3..]` holds up
/// to 13 of its bytes. `second`, when present and echoing chunk `0x02`,
/// supplies the next 14 from `second[2..]`. A length past what the chunks
/// carry is clamped, and must not panic. The bytes are joined before UTF-8
/// decoding, so a character split across the two chunks survives. `None` for
/// non-UTF-8 bytes.
fn assemble_codename(first: &[u8; 16], second: Option<&[u8; 16]>) -> Option<String> {
    let len = usize::from(first[2]);
    let mut name = first[3..3 + len.min(FIRST_CHUNK_NAME_BYTES)].to_vec();
    if let Some(second) = second.filter(|second| second[1] == 0x02) {
        let rest = len
            .saturating_sub(FIRST_CHUNK_NAME_BYTES)
            .min(SECOND_CHUNK_NAME_BYTES);
        name.extend_from_slice(&second[2..2 + rest]);
    }
    String::from_utf8(name).ok()
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

    /// The two `DeviceCodename` replies a Bolt receiver gave for an
    /// MX Anywhere 3S in slot 1 (#1962), without their report header.
    const MX_ANYWHERE_3S_FIRST: [u8; 16] = *b"\x61\x01\x0eMX Anywhere 3";
    const MX_ANYWHERE_3S_SECOND: [u8; 16] =
        *b"\x61\x02S\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

    #[test]
    fn a_long_codename_is_completed_by_its_second_chunk() {
        assert!(codename_continues(&MX_ANYWHERE_3S_FIRST));
        assert_eq!(
            assemble_codename(&MX_ANYWHERE_3S_FIRST, Some(&MX_ANYWHERE_3S_SECOND)).as_deref(),
            Some("MX Anywhere 3S")
        );
    }

    #[test]
    fn a_missing_or_unechoed_second_chunk_keeps_the_first_thirteen_bytes() {
        assert_eq!(
            assemble_codename(&MX_ANYWHERE_3S_FIRST, None).as_deref(),
            Some("MX Anywhere 3")
        );
        let mut wrong_chunk = MX_ANYWHERE_3S_SECOND;
        wrong_chunk[1] = 0x01;
        assert_eq!(
            assemble_codename(&MX_ANYWHERE_3S_FIRST, Some(&wrong_chunk)).as_deref(),
            Some("MX Anywhere 3")
        );
    }

    #[test]
    fn codename_with_oversized_length_clamps_to_the_chunks_present() {
        let mut first = MX_ANYWHERE_3S_FIRST;
        first[2] = 200;
        let mut second = [0u8; 16];
        second[1] = 0x02;
        second[2..16].copy_from_slice(b"S and the rest");

        assert_eq!(
            assemble_codename(&first, None).as_deref(),
            Some("MX Anywhere 3")
        );
        assert_eq!(
            assemble_codename(&first, Some(&second)).as_deref(),
            Some("MX Anywhere 3S and the rest")
        );
    }

    #[test]
    fn a_short_codename_needs_no_second_chunk() {
        let mut response = [0u8; 16];
        response[2] = 5;
        response[3..8].copy_from_slice(b"Casa!");

        assert!(!codename_continues(&response));
        assert_eq!(assemble_codename(&response, None).as_deref(), Some("Casa!"));
    }

    #[test]
    fn a_character_split_across_chunks_decodes() {
        let mut first = [0u8; 16];
        first[1] = 0x01;
        first[2] = 15;
        first[3..15].copy_from_slice(b"MX Ergo Plus");
        first[15] = 0xc3; // first byte of "é"
        let mut second = [0u8; 16];
        second[1] = 0x02;
        second[2] = 0xa9; // second byte of "é"
        second[3] = b'!';

        assert_eq!(
            assemble_codename(&first, Some(&second)).as_deref(),
            Some("MX Ergo Plusé!")
        );
    }

    #[test]
    fn codename_rejects_invalid_utf8() {
        let mut response = [0u8; 16];
        response[2] = 2;
        response[3] = 0xff;
        response[4] = 0xfe;

        assert_eq!(assemble_codename(&response, None), None);
    }
}
