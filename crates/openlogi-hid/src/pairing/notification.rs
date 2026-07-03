use hidpp::channel::{HidppChannel, HidppMessage, MessageListenerGuard};
use tokio::sync::mpsc;

/// Notification sub-IDs the receiver emits during pairing.
pub(super) mod id {
    pub const DEVICE_CONNECTION: u8 = 0x41;
    pub const UNIFYING_LOCK: u8 = 0x4a;
    pub const PASSKEY_REQUEST: u8 = 0x4d;
    pub const DEVICE_DISCOVERY: u8 = 0x4f;
    pub const DISCOVERY_STATUS: u8 = 0x53;
    pub const PAIRING_STATUS: u8 = 0x54;
}

/// A parsed receiver notification relevant to pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Notification {
    /// Bolt discovery address frame: kind, BTLE address, auth method.
    DiscoveryInfo {
        counter: u16,
        kind: u8,
        address: [u8; 6],
        authentication: u8,
    },
    /// Bolt discovery name frame.
    DiscoveryName { counter: u16, name: String },
    /// Bolt pairing completed; `slot` is the assigned device index.
    PairingSucceeded { slot: u8 },
    /// Bolt pairing/discovery failed with a receiver error code.
    PairingError(u8),
    /// Bolt passkey to present to the user (6 ASCII digits).
    Passkey(String),
    /// A device linked to the receiver (`slot` = its device index).
    Connected { slot: u8, established: bool },
    /// Unifying pairing lock changed state; `error` is non-zero on failure.
    UnifyingLock { open: bool, error: u8 },
}

/// Decodes a raw HID++ message into `(device_index, sub_id, payload)`, where
/// `payload[0]` is the HID++ 1.0 notification *address* byte and `payload[k]`
/// for `k >= 1` is Solaar's `data[k - 1]`. Short payloads are zero-padded.
pub(super) fn decode(msg: &HidppMessage) -> (u8, u8, [u8; 17]) {
    let mut payload = [0u8; 17];
    match msg {
        HidppMessage::Short(d) => {
            payload[..4].copy_from_slice(&d[2..6]);
            (d[0], d[1], payload)
        }
        HidppMessage::Long(d) => {
            payload.copy_from_slice(&d[2..19]);
            (d[0], d[1], payload)
        }
    }
}

/// Parses a raw message into a pairing [`Notification`], if it is one.
pub(super) fn parse_notification(
    sub_id: u8,
    device_index: u8,
    p: [u8; 17],
) -> Option<Notification> {
    match sub_id {
        id::DEVICE_CONNECTION => Some(Notification::Connected {
            slot: device_index,
            // bit 6 of the flags byte set => link not established (offline).
            established: p[1] & (1 << 6) == 0,
        }),
        id::DEVICE_DISCOVERY => {
            let counter = u16::from(p[0]) + u16::from(p[1]) * 256;
            match p[2] {
                0 => {
                    let mut address = [0u8; 6];
                    address.copy_from_slice(&p[7..13]);
                    Some(Notification::DiscoveryInfo {
                        counter,
                        kind: p[4],
                        address,
                        authentication: p[15],
                    })
                }
                1 => {
                    let len = usize::from(p[3]).min(p.len() - 4);
                    let name = String::from_utf8_lossy(&p[4..4 + len]).into_owned();
                    Some(Notification::DiscoveryName { counter, name })
                }
                _ => None,
            }
        }
        id::DISCOVERY_STATUS => {
            let error = p[1];
            if error != 0 {
                Some(Notification::PairingError(error))
            } else {
                None
            }
        }
        id::PAIRING_STATUS => {
            let error = p[1];
            if error != 0 {
                Some(Notification::PairingError(error))
            } else if p[0] == 0x02 {
                // address 0x02 with no error => paired; slot is data[7] = p[8].
                Some(Notification::PairingSucceeded { slot: p[8] })
            } else {
                None
            }
        }
        id::PASSKEY_REQUEST => {
            let passkey = String::from_utf8_lossy(&p[1..7]).into_owned();
            Some(Notification::Passkey(passkey))
        }
        id::UNIFYING_LOCK => Some(Notification::UnifyingLock {
            open: p[0] & 0x01 != 0,
            error: p[1],
        }),
        _ => None,
    }
}

/// Subscribes a listener that forwards unmatched messages to an async channel,
/// and returns the listener guard plus the receiver end.
pub(super) fn subscribe(
    channel: &HidppChannel,
) -> (MessageListenerGuard, mpsc::UnboundedReceiver<HidppMessage>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let listener = channel.add_msg_listener_guarded(move |msg, matched| {
        // `matched` messages are responses to our own register writes.
        if !matched {
            let _ = tx.send(msg);
        }
    });
    (listener, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a long HID++ message from a 17-byte payload (`p[0]` = address).
    fn long(sub_id: u8, device_index: u8, p: [u8; 17]) -> HidppMessage {
        let mut d = [0u8; 19];
        d[0] = device_index;
        d[1] = sub_id;
        d[2..19].copy_from_slice(&p);
        HidppMessage::Long(d)
    }

    #[test]
    fn decode_maps_long_payload_to_address_first() {
        let msg = long(id::DEVICE_DISCOVERY, 0xff, {
            let mut p = [0u8; 17];
            p[0] = 0x07; // counter low (= Solaar address)
            p[1] = 0x00; // counter high (= Solaar data[0])
            p
        });
        let (idx, sub, payload) = decode(&msg);
        assert_eq!(idx, 0xff);
        assert_eq!(sub, id::DEVICE_DISCOVERY);
        assert_eq!(payload[0], 0x07);
        assert_eq!(payload[1], 0x00);
    }

    #[test]
    fn parses_discovery_info_frame() {
        let mut p = [0u8; 17];
        p[0] = 0x05; // counter low
        p[1] = 0x00; // counter high
        p[2] = 0x00; // address frame selector
        p[4] = 0x02; // kind = mouse
        p[7..13].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]);
        p[15] = 0x01; // auth: keyboard-typed bit
        assert_eq!(
            parse_notification(id::DEVICE_DISCOVERY, 0xff, p),
            Some(Notification::DiscoveryInfo {
                counter: 5,
                kind: 0x02,
                address: [0xde, 0xad, 0xbe, 0xef, 0x01, 0x02],
                authentication: 0x01,
            })
        );
    }

    #[test]
    fn parses_discovery_name_frame() {
        let mut p = [0u8; 17];
        p[0] = 0x05;
        p[1] = 0x00;
        p[2] = 0x01; // name frame selector
        p[3] = 0x03; // length
        p[4..7].copy_from_slice(b"MX3");
        assert_eq!(
            parse_notification(id::DEVICE_DISCOVERY, 0xff, p),
            Some(Notification::DiscoveryName {
                counter: 5,
                name: "MX3".to_string(),
            })
        );
    }

    #[test]
    fn parses_pairing_success_with_slot() {
        let mut p = [0u8; 17];
        p[0] = 0x02; // address 0x02 = complete
        p[1] = 0x00; // no error
        p[8] = 0x03; // slot = data[7]
        assert_eq!(
            parse_notification(id::PAIRING_STATUS, 0xff, p),
            Some(Notification::PairingSucceeded { slot: 3 })
        );
    }

    #[test]
    fn parses_pairing_error() {
        let mut p = [0u8; 17];
        p[0] = 0x00;
        p[1] = 0x01; // BoltPairingError::DEVICE_TIMEOUT
        assert_eq!(
            parse_notification(id::PAIRING_STATUS, 0xff, p),
            Some(Notification::PairingError(0x01))
        );
    }

    #[test]
    fn parses_passkey_digits() {
        let mut p = [0u8; 17];
        p[1..7].copy_from_slice(b"123456");
        assert_eq!(
            parse_notification(id::PASSKEY_REQUEST, 0xff, p),
            Some(Notification::Passkey("123456".to_string()))
        );
    }

    #[test]
    fn parses_unifying_lock() {
        let mut p = [0u8; 17];
        p[0] = 0x01; // lock open
        assert_eq!(
            parse_notification(id::UNIFYING_LOCK, 0xff, p),
            Some(Notification::UnifyingLock {
                open: true,
                error: 0,
            })
        );
    }
}
