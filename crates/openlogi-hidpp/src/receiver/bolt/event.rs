//! The notifications a Bolt receiver broadcasts, and their decoding.
//!
//! The receiver reports device arrivals, discovery results, and pairing
//! progress as unsolicited HID++1.0 messages. Their layout is not publicly
//! documented — it comes from reading other implementations (primarily Solaar)
//! and from fuzzing registers — so every offset and mask here is pinned by a
//! test rather than by a specification.

use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};

use crate::{protocol::v10, receiver::RECEIVER_DEVICE_INDEX};

/// The notification sub-ids this module decodes.
///
/// Modelling the sub-id as an enum rather than matching bare bytes makes the
/// dispatch below exhaustive: a new notification cannot be added here without
/// the compiler demanding a decode for it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum Notification {
    /// A device connected to, or disconnected from, the receiver.
    DeviceConnection = 0x41,

    /// The receiver asks for a passkey to authenticate a device being paired.
    PairingPasskeyRequest = 0x4d,

    /// The user pressed a key while entering a pairing passkey.
    PairingPasskeyPressed = 0x4e,

    /// Details or the name of a device found while discovering.
    DeviceDiscovery = 0x4f,

    /// Device discovery was enabled or disabled.
    DeviceDiscoveryStatus = 0x53,
}

/// The two payload kinds a [`Notification::DeviceDiscovery`] report carries,
/// selected by `payload[2]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum DiscoveryPart {
    /// Address, kind, and product id of the discovered device.
    Details = 0,

    /// The discovered device's advertised name.
    Name = 1,
}

/// Decodes an unsolicited receiver message into the event it carries.
///
/// Returns `None` for a report this crate does not model, one addressed
/// elsewhere, or one whose payload does not parse — all of which the listener
/// drops.
///
/// Kept separate from the message listener in [`super::Receiver::new`] so the
/// wire layout is reachable from consumers and tests without a HID channel
/// behind it.
#[must_use]
pub fn decode(msg: &v10::Message) -> Option<Event> {
    let header = msg.header();
    let payload = msg.extend_payload();

    let notification = Notification::try_from(header.sub_id).ok()?;

    // Every notification but a device connection is addressed to the receiver
    // itself. A connection notification instead carries the device's slot in
    // the header — the only place that index is reported.
    if notification != Notification::DeviceConnection
        && header.device_index != RECEIVER_DEVICE_INDEX
    {
        return None;
    }

    match notification {
        Notification::DeviceConnection => Some(Event::DeviceConnection(DeviceConnection {
            index: header.device_index,
            // Kind is identity-only; an unrecognised nibble folds to `Unknown`
            // instead of dropping the event, which would hide the device
            // entirely.
            kind: DeviceKind::from(payload[1] & 0x0f),
            encrypted: payload[1] & (1 << 5) != 0,
            online: payload[1] & (1 << 6) == 0,
            wpid: u16::from_le_bytes([payload[2], payload[3]]),
        })),

        Notification::DeviceDiscovery => match DiscoveryPart::try_from(payload[2]).ok()? {
            DiscoveryPart::Details => Some(Event::DeviceDiscoveryDeviceDetails {
                counter: discovery_counter(&payload),
                kind: DeviceKind::from(payload[4] & 0x0f),
                wpid: u16::from_le_bytes([payload[5], payload[6]]),
                address: address6(&payload, 7),
                authentication: payload[15],
            }),
            DiscoveryPart::Name => {
                let name = discovery_name(&payload)?;
                Some(Event::DeviceDiscoveryDeviceName {
                    counter: discovery_counter(&payload),
                    name: name.to_string(),
                })
            }
        },

        Notification::DeviceDiscoveryStatus => Some(Event::DeviceDiscoveryStatus {
            discovery_enabled: payload[0] == 0x00,
        }),

        Notification::PairingPasskeyRequest => Some(Event::PairingPasskeyRequest {
            device_address: address6(&payload, 7),
            passkey: passkey(&payload)?.to_string(),
        }),

        Notification::PairingPasskeyPressed => Some(Event::PairingPasskeyPressed {
            device_address: address6(&payload, 1),
            press_type: PairingPasskeyPressType::from(payload[0]),
        }),
    }
}

/// The little-endian counter that pairs a discovery details report with the
/// name report describing the same device.
fn discovery_counter(payload: &[u8; 17]) -> u16 {
    u16::from_le_bytes([payload[0], payload[1]])
}

/// Extracts 6 contiguous bytes starting at `start` from a receiver-event
/// payload into a BTLE device-address array.
///
/// Every call site above passes a compile-time-fixed `start` (1, 2, or 7)
/// comfortably within the fixed 17-byte payload, so this never panics in
/// practice.
fn address6(payload: &[u8; 17], start: usize) -> [u8; 6] {
    [
        payload[start],
        payload[start + 1],
        payload[start + 2],
        payload[start + 3],
        payload[start + 4],
        payload[start + 5],
    ]
}

/// Reads the name out of a device-discovery name notification.
///
/// `payload[3]` is the device-reported name length. The byte comes straight
/// off the radio, so it must never index past the report: a length that does
/// not fit the packet (or non-UTF-8 bytes) drops the event instead of
/// panicking the listener.
fn discovery_name(payload: &[u8; 17]) -> Option<&str> {
    let end = 4usize.checked_add(usize::from(payload[3]))?;
    str::from_utf8(payload.get(4..end)?).ok()
}

/// Reads the passkey out of a passkey-request notification.
///
/// The passkey occupies 6 bytes and is NUL-padded when it is shorter.
fn passkey(payload: &[u8; 17]) -> Option<&str> {
    let digits = &payload[1..=6];
    let len = digits.iter().position(|&b| b == 0).unwrap_or(digits.len());
    str::from_utf8(&digits[..len]).ok()
}

/// Represents an event emitted by the receiver.
///
/// You can listen to these events using [`super::Receiver::listen`]. Only
/// enabled notifications as indicated by
/// [`super::Receiver::get_notification_state`] are emitted.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum Event {
    /// Is emitted whenever a device connects to or disconnects from the
    /// receiver, but only if
    /// [`NotificationState::wireless_notifications`](super::NotificationState::wireless_notifications)
    /// is enabled.
    ///
    /// Can be triggered for all paired devices using
    /// [`Receiver::trigger_device_arrival`](super::Receiver::trigger_device_arrival)
    /// to allow easy device enumeration.
    ///
    /// [`Receiver::collect_paired_devices`](super::Receiver::collect_paired_devices)
    /// implements a simple mechanism to collect all paired devices.
    DeviceConnection(DeviceConnection),

    /// Is emitted whenever the device discovery status changes.
    DeviceDiscoveryStatus {
        /// Whether discovery mode is enabled.
        discovery_enabled: bool,
    },

    /// Is emitted many times for every device discovered using
    /// [`Receiver::discover_devices`](super::Receiver::discover_devices).
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
        /// [`Receiver::pair_device`](super::Receiver::pair_device).
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
    /// [`Receiver::discover_devices`](super::Receiver::discover_devices).
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
        /// passed to [`Receiver::pair_device`](super::Receiver::pair_device).
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
/// be conveniently collected using
/// [`Receiver::collect_paired_devices`](super::Receiver::collect_paired_devices).
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
    use super::{
        DeviceConnection, DeviceKind, Event, PairingPasskeyPressType, decode, discovery_name,
    };
    use crate::{
        protocol::v10::{Message, MessageHeader},
        receiver::RECEIVER_DEVICE_INDEX,
    };

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

    /// A receiver-addressed notification.
    fn from_receiver(sub_id: u8, payload: [u8; 17]) -> Message {
        notification(RECEIVER_DEVICE_INDEX, sub_id, payload)
    }

    #[test]
    fn device_connection_reads_slot_from_the_header_not_the_payload() {
        // A connection notification is the only one addressed to the device's
        // own slot rather than to the receiver, and that header byte is the
        // only place the slot is reported.
        let mut payload = [0u8; 17];
        payload[1] = 0x02; // mouse, not encrypted, online
        payload[2] = 0x0b;
        payload[3] = 0x40;

        let event = decode(&notification(3, 0x41, payload)).unwrap();

        assert_eq!(
            event,
            Event::DeviceConnection(DeviceConnection {
                index: 3,
                kind: DeviceKind::Mouse,
                encrypted: false,
                online: true,
                wpid: 0x400b,
            })
        );
    }

    #[test]
    fn device_connection_decodes_its_status_bits() {
        // Bit 5 is the link encryption flag and bit 6 is *inverted*: it is set
        // when the device is offline. Unifying uses the same layout — bit 4 is
        // the software-present flag on both receivers.
        let connection = |status: u8| {
            let mut payload = [0u8; 17];
            payload[1] = status;
            match decode(&notification(1, 0x41, payload)) {
                Some(Event::DeviceConnection(connection)) => connection,
                other => panic!("expected a device connection, got {other:?}"),
            }
        };

        let encrypted_online = connection(1 << 5);
        assert!(encrypted_online.encrypted);
        assert!(encrypted_online.online);

        let plain_offline = connection(1 << 6);
        assert!(!plain_offline.encrypted);
        assert!(!plain_offline.online);
    }

    #[test]
    fn unmodelled_device_kind_folds_to_unknown_instead_of_dropping_the_event() {
        // Losing the event would hide the device from enumeration entirely,
        // which is far worse than reporting an unknown kind.
        let mut payload = [0u8; 17];
        payload[1] = 0x0e;

        let Some(Event::DeviceConnection(connection)) = decode(&notification(1, 0x41, payload))
        else {
            panic!("an unknown kind must still produce an event");
        };
        assert_eq!(connection.kind, DeviceKind::Unknown);
    }

    #[test]
    fn discovery_details_and_name_share_a_counter() {
        // The counter is what lets a caller join the two halves of one
        // discovered device, so both must read it from the same little-endian
        // pair.
        let mut details = [0u8; 17];
        details[0] = 0x34;
        details[1] = 0x12;
        details[2] = 0; // details part
        details[4] = 0x01; // keyboard
        details[5] = 0xcd;
        details[6] = 0xab;
        details[7..13].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        details[15] = 0x20;

        assert_eq!(
            decode(&from_receiver(0x4f, details)).unwrap(),
            Event::DeviceDiscoveryDeviceDetails {
                counter: 0x1234,
                kind: DeviceKind::Keyboard,
                wpid: 0xabcd,
                address: [1, 2, 3, 4, 5, 6],
                authentication: 0x20,
            }
        );

        let mut name = [0u8; 17];
        name[0] = 0x34;
        name[1] = 0x12;
        name[2] = 1; // name part
        name[3] = 4;
        name[4..8].copy_from_slice(b"Casa");

        assert_eq!(
            decode(&from_receiver(0x4f, name)).unwrap(),
            Event::DeviceDiscoveryDeviceName {
                counter: 0x1234,
                name: "Casa".to_string(),
            }
        );
    }

    #[test]
    fn unmodelled_discovery_part_is_dropped() {
        let mut payload = [0u8; 17];
        payload[2] = 9;

        assert_eq!(decode(&from_receiver(0x4f, payload)), None);
    }

    #[test]
    fn discovery_status_is_inverted_on_the_wire() {
        let enabled = |byte: u8| {
            let mut payload = [0u8; 17];
            payload[0] = byte;
            decode(&from_receiver(0x53, payload)).unwrap()
        };

        assert_eq!(
            enabled(0x00),
            Event::DeviceDiscoveryStatus {
                discovery_enabled: true
            }
        );
        assert_eq!(
            enabled(0x01),
            Event::DeviceDiscoveryStatus {
                discovery_enabled: false
            }
        );
    }

    #[test]
    fn passkey_request_stops_at_the_nul_padding() {
        let mut payload = [0u8; 17];
        payload[1..5].copy_from_slice(b"1234");
        payload[7..13].copy_from_slice(&[0xaa; 6]);

        assert_eq!(
            decode(&from_receiver(0x4d, payload)).unwrap(),
            Event::PairingPasskeyRequest {
                device_address: [0xaa; 6],
                passkey: "1234".to_string(),
            }
        );
    }

    #[test]
    fn passkey_request_uses_all_six_digits_when_unpadded() {
        let mut payload = [0u8; 17];
        payload[1..7].copy_from_slice(b"951753");

        let Some(Event::PairingPasskeyRequest { passkey, .. }) =
            decode(&from_receiver(0x4d, payload))
        else {
            panic!("expected a passkey request");
        };
        assert_eq!(passkey, "951753");
    }

    #[test]
    fn passkey_request_with_invalid_utf8_is_dropped() {
        let mut payload = [0u8; 17];
        payload[1] = 0xff;
        payload[2] = 0xfe;

        assert_eq!(decode(&from_receiver(0x4d, payload)), None);
    }

    #[test]
    fn passkey_press_carries_an_unmodelled_press_type() {
        let mut payload = [0u8; 17];
        payload[0] = 0x33;
        payload[1..7].copy_from_slice(&[1, 2, 3, 4, 5, 6]);

        assert_eq!(
            decode(&from_receiver(0x4e, payload)).unwrap(),
            Event::PairingPasskeyPressed {
                device_address: [1, 2, 3, 4, 5, 6],
                press_type: PairingPasskeyPressType::Other(0x33),
            }
        );
    }

    #[test]
    fn notifications_addressed_elsewhere_are_dropped_except_device_connections() {
        // A device-addressed report is only ever a connection notification;
        // anything else at a device index belongs to that device, not to us.
        assert_eq!(decode(&notification(2, 0x53, [0u8; 17])), None);
        assert!(decode(&notification(2, 0x41, [0u8; 17])).is_some());
    }

    #[test]
    fn unmodelled_sub_id_is_dropped() {
        assert_eq!(decode(&from_receiver(0x42, [0u8; 17])), None);
    }

    #[test]
    fn short_notifications_decode_from_the_zero_padded_payload() {
        // The receiver may answer in either report width; a short report is
        // widened with zeroes, which must not change the decoded event.
        let short = Message::Short(
            MessageHeader {
                device_index: 4,
                sub_id: 0x41,
            },
            [0x00, 0x02, 0x0b, 0x40],
        );

        assert_eq!(
            decode(&short).unwrap(),
            Event::DeviceConnection(DeviceConnection {
                index: 4,
                kind: DeviceKind::Mouse,
                encrypted: false,
                online: true,
                wpid: 0x400b,
            })
        );
    }

    #[test]
    fn discovery_name_with_oversized_length_is_dropped() {
        let mut payload = [0u8; 17];
        payload[3] = 200;

        assert_eq!(discovery_name(&payload), None);
    }

    #[test]
    fn discovery_name_within_bounds_parses() {
        let mut payload = [0u8; 17];
        payload[3] = 4;
        payload[4..8].copy_from_slice(b"Casa");

        assert_eq!(discovery_name(&payload), Some("Casa"));
    }

    #[test]
    fn discovery_name_rejects_invalid_utf8() {
        let mut payload = [0u8; 17];
        payload[3] = 2;
        payload[4] = 0xff;
        payload[5] = 0xfe;

        assert_eq!(discovery_name(&payload), None);
    }
}
