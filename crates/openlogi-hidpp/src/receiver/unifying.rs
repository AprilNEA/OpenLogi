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

use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};
use openlogi_device_registry::receiver::{ReceiverProtocol, find_receiver};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    emitter::EventEmitter,
    protocol::v10,
    receiver::{RECEIVER_DEVICE_INDEX, ReceiverError},
};

/// All known registers of the Unifying receiver.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum Register {
    /// Controls which notifications the receiver emits. Wireless device-arrival
    /// (`0x41`) events are only re-broadcast while wireless notifications are
    /// enabled here; see [`Receiver::set_wireless_notifications`].
    Notifications = 0x00,

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
    ///
    /// NOTE: `0x60` is the *Bolt* base. Wire-verified Unifying receivers store
    /// names at base `0x40 + (n-1)` instead, so name reads go directly through
    /// `read_codename_unifying` in `inventory.rs` rather than this constant —
    /// don't reuse `DeviceCodename` for Unifying name reads.
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
        if find_receiver(chan.vendor_id, chan.product_id)
            .is_none_or(|receiver| receiver.protocol != ReceiverProtocol::Unifying)
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

                if let Some(event) = decode_notification(&v10::Message::from(raw)) {
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

    /// Enables or disables wireless device-connection notifications.
    ///
    /// The receiver only re-broadcasts `0x41` device-arrival events (the source
    /// for [`Self::trigger_device_arrival`]) while this is on. With it off the
    /// trigger write is ACK'd but emits nothing — which is why a paired, online
    /// device can fail to enumerate. Solaar enables this before listing.
    ///
    /// Read-modify-write of just the `WIRELESS` bit so it can't clobber other
    /// flags already set on register `0x00` — notably `SOFTWARE_PRESENT` (0x08),
    /// which the pairing flow enables (`pairing.rs` writes `[0x00, 0x09, 0x00]`)
    /// and a concurrent inventory poll would otherwise drop.
    pub async fn set_wireless_notifications(&self, enabled: bool) -> Result<(), ReceiverError> {
        // Notification flags are a 3-byte big-endian word; the receiver-reporting
        // bits live in byte 1 (WIRELESS = 0x000100, SOFTWARE_PRESENT = 0x000800).
        let mut flags = self
            .chan
            .read_register(
                RECEIVER_DEVICE_INDEX,
                Register::Notifications.into(),
                [0; 3],
            )
            .await?;
        // This flag persists in receiver RAM. Avoid issuing an identical
        // register write on every inventory tick: Lightspeed receiver c54d
        // has been observed to occasionally omit the ACK for that no-op,
        // parking the otherwise healthy shared channel until timeout.
        if !update_wireless_notification_flag(&mut flags, enabled) {
            return Ok(());
        }
        self.chan
            .write_register(RECEIVER_DEVICE_INDEX, Register::Notifications.into(), flags)
            .await?;

        Ok(())
    }

    /// Triggers device-arrival notifications for every paired slot, online or
    /// not — the notification's link-status bit distinguishes (Solaar uses the
    /// same trigger as its "scan all devices" pass). Used to enumerate paired
    /// devices at startup.
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
            wpid: u16::from_le_bytes([response[2], response[3]]),
            // Kind is identity-only: an unrecognised nibble folds to
            // `Unknown` instead of failing the whole pairing-info read.
            kind: DeviceKind::from(response[1] & 0x0f),
            encrypted: response[1] & (1 << 5) != 0,
            online: response[1] & (1 << 6) == 0,
            unit_id: [response[4], response[5], response[6], response[7]],
        })
    }

    /// Provides the unique ID of the receiver (serial number).
    pub async fn get_unique_id(&self) -> Result<String, ReceiverError> {
        self.get_receiver_info().await.map(|i| i.serial_number)
    }
}

/// Update the wireless-notification bit and report whether a register write is
/// needed. Kept separate so the preservation of unrelated flags is testable
/// without a receiver transport.
fn update_wireless_notification_flag(flags: &mut [u8; 3], enabled: bool) -> bool {
    const WIRELESS: u8 = 0x01;
    let previous = *flags;
    if enabled {
        flags[1] |= WIRELESS;
    } else {
        flags[1] &= !WIRELESS;
    }
    *flags != previous
}

/// The sub-id of the only notification this receiver emits: a paired slot's
/// connection status changed, or was re-reported by
/// [`Receiver::trigger_device_arrival`].
const DEVICE_CONNECTION_SUB_ID: u8 = 0x41;

/// Decodes an unsolicited receiver message into the event it carries, or
/// `None` for a report this crate does not model.
///
/// Public so consumers can decode captured reports and fabricate events from
/// wire bytes in their own tests without a HID channel behind them.
#[must_use]
pub fn decode_notification(msg: &v10::Message) -> Option<Event> {
    let header = msg.header();
    if header.sub_id != DEVICE_CONNECTION_SUB_ID {
        return None;
    }
    let payload = msg.extend_payload();

    // A connection notification is addressed to the device's own slot, which
    // is the only place that index is reported.
    Some(Event::DeviceConnection(DeviceConnection {
        index: header.device_index,
        // Kind is identity-only; an unrecognised nibble folds to `Unknown` —
        // dropping the event would hide the device entirely, since arrival
        // notifications are the only device source on this path.
        kind: DeviceKind::from(payload[1] & 0x0f),
        // Device-info high nibble: bit 6 = link not established, bit 5 = link
        // encrypted, bit 4 = software present (same layout as Bolt; Solaar
        // decodes both receivers with one mask table).
        encrypted: payload[1] & (1 << 5) != 0,
        online: payload[1] & (1 << 6) == 0,
        wpid: u16::from_le_bytes([payload[2], payload[3]]),
    }))
}

/// Represents some general information about a Unifying receiver.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ReceiverInfo {
    /// Receiver serial number.
    pub serial_number: String,
    /// Number of available pairing slots.
    pub pairing_slots: u8,
}

/// Represents information about a paired device as read from the pairing
/// register.
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

/// Represents the kind of a device paired to a Unifying receiver.
///
/// The encoding matches Bolt for values 1–4; from 5 onwards Unifying uses a
/// shifted table (Remote=5, Trackball=6, Touchpad=7) while Bolt reserves those
/// values and places them at 7–9.
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
    Remote = 0x05,
    /// Trackball device.
    Trackball = 0x06,
    /// Touchpad device.
    Touchpad = 0x07,
}

/// Represents a device-connection event fired by the receiver when a paired
/// device's link status changes, or re-broadcast for a paired slot in
/// response to [`Receiver::trigger_device_arrival`].
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
    /// Whether the device's link is currently established (payload bit 6
    /// clear). Trigger-driven re-broadcasts report offline paired slots with
    /// `false`, so a `0x41` alone is a slot report, not proof of liveness.
    pub online: bool,
    /// Wireless product ID of the device.
    pub wpid: u16,
}

/// Represents an event emitted by the Unifying receiver.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Event {
    /// Fired whenever a paired device connects or reconnects, and for *every*
    /// paired slot — offline ones included — in response to
    /// [`Receiver::trigger_device_arrival`], with
    /// [`DeviceConnection::online`] carrying the link status.
    DeviceConnection(DeviceConnection),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        DeviceConnection, DeviceKind, DevicePairingInformation, Event, InfoSubRegister, Receiver,
        Register, decode_notification, update_wireless_notification_flag,
    };
    use crate::channel::tests::{MockRawHidChannel, channel_with_reader};
    use crate::protocol::v10::{Message, MessageHeader, MessageType};

    /// Builds the long notification the receiver broadcasts, with `payload`
    /// laid out exactly as the 17 bytes following the header.
    fn notification(device_index: u8, sub_id: u8, payload: [u8; 17]) -> Message {
        Message::Long(
            MessageHeader {
                device_index,
                sub_id,
            },
            payload,
        )
    }

    #[test]
    fn wireless_notification_flag_only_writes_on_a_real_change() {
        let mut disabled = [0x00, 0x08, 0x55];
        assert!(update_wireless_notification_flag(&mut disabled, true));
        assert_eq!(disabled, [0x00, 0x09, 0x55]);
        assert!(!update_wireless_notification_flag(&mut disabled, true));

        assert!(update_wireless_notification_flag(&mut disabled, false));
        assert_eq!(disabled, [0x00, 0x08, 0x55]);
        assert!(!update_wireless_notification_flag(&mut disabled, false));
    }

    #[test]
    fn device_connection_reads_the_slot_from_the_header() {
        // The header byte is the only place the slot is reported.
        let mut payload = [0u8; 17];
        payload[1] = 0x02; // mouse, not encrypted, online
        payload[2] = 0x74;
        payload[3] = 0x40;

        assert_eq!(
            decode_notification(&notification(5, 0x41, payload)).unwrap(),
            Event::DeviceConnection(DeviceConnection {
                index: 5,
                kind: DeviceKind::Mouse,
                encrypted: false,
                online: true,
                wpid: 0x4074,
            })
        );
    }

    #[test]
    fn encryption_sits_on_bit_5_and_bit_4_is_software_present() {
        // Both receivers report link encryption on bit 5 of the device-info
        // byte; bit 4 is the software-present flag. This decoder used to read
        // bit 4, reporting "Options+ seen" as link encryption.
        let connection = |status: u8| {
            let mut payload = [0u8; 17];
            payload[1] = status;
            match decode_notification(&notification(1, 0x41, payload)) {
                Some(Event::DeviceConnection(connection)) => connection,
                other => panic!("expected a device connection, got {other:?}"),
            }
        };

        assert!(connection(1 << 5).encrypted);
        assert!(!connection(1 << 4).encrypted);
    }

    #[test]
    fn pairing_information_reads_encryption_from_bit_5() {
        // The pairing register carries the same device-info byte as the 0x41
        // notification, decoded independently — pin its bits too so the two
        // paths cannot diverge unnoticed.
        futures::executor::block_on(async {
            let (raw, handle) = MockRawHidChannel::new();
            let chan = Arc::new(channel_with_reader(raw).await);
            let receiver =
                Receiver::new(chan).expect("the mock's VID/PID routes as a Unifying receiver");

            // First response: bit 5 + bit 6 — encrypted, offline. Second:
            // bit 4 only (software present), which must not read as
            // encryption.
            for status in [0x62, 0x12] {
                let mut payload = [0u8; 17];
                payload[0] = Register::ReceiverInfo.into(); // RAP matches on the address echo
                payload[1] = u8::from(InfoSubRegister::DevicePairingInformation) | 0x02;
                payload[2] = status;
                payload[3] = 0x69; // wpid, little-endian
                payload[4] = 0x40;
                payload[5..9].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
                handle.queue_response(
                    Message::Long(
                        MessageHeader {
                            device_index: super::RECEIVER_DEVICE_INDEX,
                            sub_id: MessageType::GetLongRegister.into(),
                        },
                        payload,
                    )
                    .into(),
                );
            }

            let encrypted_offline = receiver.get_device_pairing_information(2).await.unwrap();
            assert_eq!(
                encrypted_offline,
                DevicePairingInformation {
                    wpid: 0x4069,
                    kind: DeviceKind::Mouse,
                    encrypted: true,
                    online: false,
                    unit_id: [0xde, 0xad, 0xbe, 0xef],
                }
            );

            let software_present = receiver.get_device_pairing_information(2).await.unwrap();
            assert!(
                !software_present.encrypted,
                "bit 4 is software-present, not encryption"
            );
            assert!(software_present.online);
        });
    }

    #[test]
    fn bit_6_is_set_when_the_device_is_offline() {
        let mut payload = [0u8; 17];
        payload[1] = 1 << 6;

        let Some(Event::DeviceConnection(connection)) =
            decode_notification(&notification(1, 0x41, payload))
        else {
            panic!("expected a device connection");
        };
        assert!(!connection.online);
    }

    #[test]
    fn device_kind_uses_the_unifying_table_not_bolts() {
        // Unifying and Bolt agree up to 4 and diverge from 5 on: `5` is a
        // remote here but reserved on Bolt, which places its remote at 7.
        let kind = |nibble: u8| {
            let mut payload = [0u8; 17];
            payload[1] = nibble;
            match decode_notification(&notification(1, 0x41, payload)) {
                Some(Event::DeviceConnection(connection)) => connection.kind,
                other => panic!("expected a device connection, got {other:?}"),
            }
        };

        assert_eq!(kind(0x05), DeviceKind::Remote);
        assert_eq!(kind(0x06), DeviceKind::Trackball);
        assert_eq!(kind(0x07), DeviceKind::Touchpad);
    }

    #[test]
    fn unmodelled_device_kind_folds_to_unknown_instead_of_dropping_the_event() {
        // Losing the event would hide the device from enumeration entirely,
        // and arrival notifications are the only device source on this path.
        let mut payload = [0u8; 17];
        payload[1] = 0x0d;

        let Some(Event::DeviceConnection(connection)) =
            decode_notification(&notification(1, 0x41, payload))
        else {
            panic!("an unknown kind must still produce an event");
        };
        assert_eq!(connection.kind, DeviceKind::Unknown);
    }

    #[test]
    fn other_sub_ids_are_dropped() {
        assert_eq!(decode_notification(&notification(1, 0x40, [0u8; 17])), None);
        assert_eq!(decode_notification(&notification(1, 0x4f, [0u8; 17])), None);
    }

    #[test]
    fn short_notifications_decode_from_the_zero_padded_payload() {
        let short = Message::Short(
            MessageHeader {
                device_index: 2,
                sub_id: 0x41,
            },
            [0x00, 0x01, 0x74, 0x40],
        );

        assert_eq!(
            decode_notification(&short).unwrap(),
            Event::DeviceConnection(DeviceConnection {
                index: 2,
                kind: DeviceKind::Keyboard,
                encrypted: false,
                online: true,
                wpid: 0x4074,
            })
        );
    }
}
