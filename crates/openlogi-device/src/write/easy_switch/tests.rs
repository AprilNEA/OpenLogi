//! Tests for the `0x1815` Easy-Switch reads and the host rename.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

/// The `0x1815` feature index on the scripted keyboard.
const HOSTS_INDEX: u8 = 0x0c;
/// HID++ 2.0 `InvalidFunctionId`.
const INVALID_FUNCTION: u8 = 0x07;
/// HID++ 2.0 `Busy`.
const BUSY: u8 = 0x08;

/// One host slot as the scripted keyboard stores it.
#[derive(Clone)]
struct Slot {
    /// `getHostInfo` status: 0 empty, 1 paired.
    status: u8,
    /// `getHostInfo` bus type.
    bus: u8,
    /// The stored name buffer, `max` bytes long.
    name: Vec<u8>,
    /// The name length `getHostInfo` reports.
    name_len: u8,
    /// The longest name the slot stores.
    max: u8,
    /// `getHostOsVersion` bytes after the host index.
    os: [u8; 6],
}

impl Slot {
    fn paired(bus: u8, name: &str, max: u8, os: [u8; 6]) -> Self {
        let mut buffer = name.as_bytes().to_vec();
        buffer.resize(usize::from(max), 0);
        Self {
            status: 1,
            bus,
            name: buffer,
            name_len: u8::try_from(name.len()).expect("a short test name"),
            max,
            os,
        }
    }

    fn empty() -> Self {
        Self {
            status: 0,
            bus: 0,
            name: vec![0; 24],
            name_len: 0,
            max: 24,
            os: [0; 6],
        }
    }
}

/// A three-slot Easy-Switch keyboard.
struct Keyboard {
    /// `getFeatureInfos` byte 0.
    capabilities: u8,
    /// `getFeatureInfos` byte 3.
    current: u8,
    slots: Vec<Slot>,
    /// Functions answered with `InvalidFunctionId`.
    declines: Vec<u8>,
    /// A function answered with `Busy`.
    busy: Option<u8>,
    /// Whether a name write changes the stored name.
    takes_writes: bool,
    /// A function, 3 or 7, whose replies name the next slot.
    echoes_next_slot: Option<u8>,
    /// Whether name replies name the next chunk's offset.
    echoes_next_offset: bool,
    /// Every name read, as `(slot, offset)`.
    name_reads: Vec<(u8, u8)>,
    /// Every OS read, by slot.
    os_reads: Vec<u8>,
    /// Every name write, as `(slot, offset, bytes)`.
    writes: Vec<(u8, u8, Vec<u8>)>,
}

/// macOS 15.2.3.
const MACOS_15_2: [u8; 6] = [6, 15, 0, 2, 0, 3];
/// Windows 10.0.
const WINDOWS_10: [u8; 6] = [1, 10, 0, 0, 0, 0];

impl Keyboard {
    fn new() -> Self {
        Self {
            // GET_NAME | SET_NAME.
            capabilities: 0b0000_0011,
            current: 0,
            slots: vec![
                Slot::paired(4, "MacBook Pro", 24, MACOS_15_2),
                Slot::paired(5, "Workstation-Win11-PC", 30, WINDOWS_10),
                Slot::empty(),
            ],
            declines: Vec::new(),
            busy: None,
            takes_writes: true,
            echoes_next_slot: None,
            echoes_next_offset: false,
            name_reads: Vec::new(),
            os_reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let function = request[3] >> 4;
        if request[2] == HOSTS_INDEX {
            if self.declines.contains(&function) {
                return Some(feature_error(request, INVALID_FUNCTION));
            }
            if self.busy == Some(function) {
                return Some(feature_error(request, BUSY));
            }
        }
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                if u16::from_be_bytes([request[4], request[5]]) == 0x1815 {
                    payload[0] = HOSTS_INDEX;
                }
            }
            (HOSTS_INDEX, 0x00) => {
                payload[..4].copy_from_slice(&[
                    self.capabilities,
                    0b1000,
                    u8::try_from(self.slots.len()).expect("a few slots"),
                    self.current,
                ]);
            }
            (HOSTS_INDEX, 0x01) => {
                let slot = self.slots.get(usize::from(request[4]))?;
                payload[..6].copy_from_slice(&[
                    request[4],
                    slot.status,
                    slot.bus,
                    1,
                    slot.name_len,
                    slot.max,
                ]);
            }
            (HOSTS_INDEX, 0x03) => {
                let (index, offset) = (request[4], request[5]);
                self.name_reads.push((index, offset));
                let slot = self.slots.get(usize::from(index))?;
                payload[0] = index + u8::from(self.echoes_next_slot == Some(0x03));
                payload[1] = offset + 14 * u8::from(self.echoes_next_offset);
                for (to, from) in payload[2..]
                    .iter_mut()
                    .zip(slot.name.iter().skip(offset.into()))
                {
                    *to = *from;
                }
            }
            (HOSTS_INDEX, 0x04) => {
                let (index, offset) = (request[4], request[5]);
                let bytes = request[6..20].to_vec();
                self.writes.push((index, offset, bytes.clone()));
                if self.takes_writes {
                    let slot = self.slots.get_mut(usize::from(index))?;
                    for (to, from) in slot.name.iter_mut().skip(offset.into()).zip(bytes) {
                        *to = from;
                    }
                    // Report the whole buffer, so only the zero padding ends
                    // the name.
                    slot.name_len = slot.max;
                }
            }
            (HOSTS_INDEX, 0x07) => {
                self.os_reads.push(request[4]);
                let slot = self.slots.get(usize::from(request[4]))?;
                payload[0] = request[4] + u8::from(self.echoes_next_slot == Some(0x07));
                payload[1..7].copy_from_slice(&slot.os);
            }
            _ => return None,
        }
        let mut response = vec![0u8; 20];
        response[0] = 0x11;
        response[1..4].copy_from_slice(&request[1..4]);
        response[4..].copy_from_slice(&payload);
        Some(response)
    }
}

async fn keyboard_channel(keyboard: &Arc<Mutex<Keyboard>>) -> SharedChannel {
    let keyboard = Arc::clone(keyboard);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        keyboard.lock().expect("keyboard lock").respond(request)
    });
    SharedChannel::new(
        scripted_channel(raw).await,
        DeviceRoute::Bolt {
            receiver_uid: "BOLT".to_string(),
            slot: 1,
        },
    )
}

fn slot(
    index: u8,
    connection: HostConnection,
    name: Option<&str>,
    name_max_len: u8,
    os: Option<HostOsInfo>,
) -> EasySwitchSlot {
    EasySwitchSlot {
        index,
        paired: name.is_some(),
        connection,
        name: name.map(str::to_owned),
        name_max_len,
        os,
    }
}

fn os(kind: HostOsKind, major: u8, minor: u16, patch: u16) -> HostOsInfo {
    HostOsInfo {
        kind,
        major,
        minor,
        patch,
    }
}

#[tokio::test]
async fn reads_every_slot_with_its_name_and_os() {
    let keyboard = Arc::new(Mutex::new(Keyboard {
        current: 1,
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&keyboard).await;
    let hosts = get_easy_switch_hosts_on(&shared)
        .await
        .expect("the keyboard answers");
    assert_eq!(
        hosts,
        EasySwitchHosts {
            current: Some(1),
            slots: vec![
                slot(
                    0,
                    HostConnection::Bluetooth,
                    Some("MacBook Pro"),
                    24,
                    Some(os(HostOsKind::MacOs, 15, 2, 3)),
                ),
                slot(
                    1,
                    HostConnection::Bolt,
                    Some("Workstation-Win11-PC"),
                    30,
                    Some(os(HostOsKind::Windows, 10, 0, 0)),
                ),
                slot(2, HostConnection::Unknown, None, 24, None),
            ],
            can_rename: true,
        }
    );
    let keyboard = keyboard.lock().expect("keyboard lock");
    // A 20-byte name takes a second chunk at byte offset 14; the empty slot
    // is not asked for a name or an OS.
    assert_eq!(keyboard.name_reads, [(0, 0), (1, 0), (1, 14)]);
    assert_eq!(keyboard.os_reads, [0, 1]);
}

#[tokio::test]
async fn a_name_or_os_the_keyboard_declines_to_report_is_left_out() {
    let keyboard = Arc::new(Mutex::new(Keyboard {
        // 0xff names no slot: the keyboard does not say which is current.
        current: 0xff,
        declines: vec![0x03, 0x07],
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&keyboard).await;
    let hosts = get_easy_switch_hosts_on(&shared)
        .await
        .expect("a declined name or OS does not fail the read");
    assert_eq!(hosts.current, None);
    assert!(hosts.slots[0].paired && hosts.slots[1].paired);
    assert!(
        hosts
            .slots
            .iter()
            .all(|slot| slot.name.is_none() && slot.os.is_none())
    );
}

#[tokio::test]
async fn an_unknown_os_is_no_os() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").slots[0].os = [0, 0, 0, 0, 0, 0];
    keyboard.lock().expect("keyboard lock").slots[1].os = [0x42, 1, 0, 0, 0, 0];
    let shared = keyboard_channel(&keyboard).await;
    let hosts = get_easy_switch_hosts_on(&shared)
        .await
        .expect("the keyboard answers");
    assert_eq!(hosts.slots[0].os, None);
    assert_eq!(hosts.slots[1].os, None);
}

#[tokio::test]
async fn a_busy_name_read_fails_the_read() {
    let keyboard = Arc::new(Mutex::new(Keyboard {
        busy: Some(0x03),
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&keyboard).await;
    assert_matches!(
        get_easy_switch_hosts_on(&shared).await,
        Err(WriteError::HidppFeature {
            operation: HidppOperation::ReadHosts,
            feature_hex: 0x1815,
            ..
        })
    );
}

#[tokio::test]
async fn a_rename_writes_the_whole_buffer_zero_padded_and_reads_it_back() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    let hosts = rename_easy_switch_host_on(&shared, 1, "  Studio ")
        .await
        .expect("the keyboard keeps the name");
    assert_eq!(hosts.slots[1].name.as_deref(), Some("Studio"));
    let keyboard = keyboard.lock().expect("keyboard lock");
    let mut first = b"Studio".to_vec();
    first.resize(14, 0);
    // 30 bytes in 14-byte chunks: offsets 0, 14 and 28, the tail zeroed so
    // nothing of "Workstation-Win11-PC" survives.
    assert_eq!(
        keyboard.writes,
        [(1, 0, first), (1, 14, vec![0; 14]), (1, 28, vec![0; 14])]
    );
    assert_eq!(&keyboard.slots[1].name[..6], b"Studio");
    assert!(keyboard.slots[1].name[6..].iter().all(|&byte| byte == 0));
}

#[tokio::test]
async fn a_name_longer_than_one_chunk_spans_chunks() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    let name = "Living-room-iMac-2024";
    let hosts = rename_easy_switch_host_on(&shared, 0, name)
        .await
        .expect("the keyboard keeps the name");
    assert_eq!(hosts.slots[0].name.as_deref(), Some(name));
    let keyboard = keyboard.lock().expect("keyboard lock");
    let offsets: Vec<u8> = keyboard.writes.iter().map(|write| write.1).collect();
    assert_eq!(offsets, [0, 14]);
    assert_eq!(keyboard.writes[0].2, name.as_bytes()[..14]);
    let mut tail = name.as_bytes()[14..].to_vec();
    tail.resize(14, 0);
    assert_eq!(keyboard.writes[1].2, tail);
}

#[tokio::test]
async fn renaming_to_the_current_name_writes_nothing() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    let hosts = rename_easy_switch_host_on(&shared, 0, "MacBook Pro")
        .await
        .expect("nothing to change");
    assert_eq!(hosts.slots[0].name.as_deref(), Some("MacBook Pro"));
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn a_rename_is_checked_before_anything_is_written() {
    let read_only = Arc::new(Mutex::new(Keyboard {
        // GET_NAME only.
        capabilities: 0b0000_0001,
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&read_only).await;
    assert_eq!(
        rename_easy_switch_host_on(&shared, 0, "Studio").await,
        Err(WriteError::HostRenameUnsupported)
    );
    assert!(read_only.lock().expect("keyboard lock").writes.is_empty());

    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    assert_eq!(
        rename_easy_switch_host_on(&shared, 2, "Studio").await,
        Err(WriteError::HostSlotEmpty { slot: 2 })
    );
    assert_eq!(
        rename_easy_switch_host_on(&shared, 3, "Studio").await,
        Err(WriteError::HostSlotEmpty { slot: 3 })
    );
    assert_eq!(
        rename_easy_switch_host_on(&shared, 0, &"x".repeat(25)).await,
        Err(WriteError::HostNameRejected { max_len: 24 })
    );
    assert_eq!(
        rename_easy_switch_host_on(&shared, 0, " ").await,
        Err(WriteError::HostNameRejected { max_len: 24 })
    );
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn a_name_the_keyboard_does_not_keep_fails_the_rename() {
    let keyboard = Arc::new(Mutex::new(Keyboard {
        takes_writes: false,
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&keyboard).await;
    assert_eq!(
        rename_easy_switch_host_on(&shared, 0, "Studio").await,
        Err(WriteError::HostNameNotApplied {
            requested: "Studio".to_owned(),
            actual: Some("MacBook Pro".to_owned()),
        })
    );
}

#[tokio::test]
async fn a_failed_name_write_fails_as_a_rename() {
    let keyboard = Arc::new(Mutex::new(Keyboard {
        busy: Some(0x04),
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&keyboard).await;
    assert_matches!(
        rename_easy_switch_host_on(&shared, 0, "Studio").await,
        Err(WriteError::HidppFeature {
            operation: HidppOperation::RenameHost,
            feature_hex: 0x1815,
            ..
        })
    );
}

#[test]
fn a_name_ends_at_its_first_zero_byte() {
    assert_eq!(decode_name(b"Desk\0junk"), Some("Desk".to_owned()));
    assert_eq!(decode_name(b"Desk"), Some("Desk".to_owned()));
    assert_eq!(decode_name(b"\0Desk"), None);
    assert_eq!(decode_name(b""), None);
    assert_eq!(decode_name("Café".as_bytes()), Some("Café".to_owned()));
    assert_eq!(decode_name(b"bad\xff"), Some("bad\u{fffd}".to_owned()));
}

#[test]
fn bluetooth_classic_and_le_are_one_connection() {
    assert_eq!(connection(HostBusType::Bt), HostConnection::Bluetooth);
    assert_eq!(connection(HostBusType::Ble), HostConnection::Bluetooth);
    assert_eq!(connection(HostBusType::BlePro), HostConnection::Bolt);
    assert_eq!(connection(HostBusType::Equad), HostConnection::Unifying);
    assert_eq!(connection(HostBusType::Usb), HostConnection::Usb);
    assert_eq!(connection(HostBusType::Undefined), HostConnection::Unknown);
    assert_eq!(os_kind(HostOsType::Unknown), None);
    assert_eq!(os_kind(HostOsType::Chrome), Some(HostOsKind::ChromeOs));
}

#[tokio::test]
async fn a_name_stops_at_the_length_the_keyboard_reports() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    {
        let mut keyboard = keyboard.lock().expect("keyboard lock");
        // The buffer still holds the tail of an older, longer name.
        keyboard.slots[0] = Slot::paired(4, "Studio MacBook Pro", 24, MACOS_15_2);
        keyboard.slots[0].name_len = 6;
    }
    let shared = keyboard_channel(&keyboard).await;

    let hosts = get_easy_switch_hosts_on(&shared).await.expect("the read");

    assert_eq!(hosts.slots[0].name.as_deref(), Some("Studio"));
}

#[tokio::test]
async fn a_current_slot_outside_the_slots_is_no_current_slot() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").current = 3;
    let shared = keyboard_channel(&keyboard).await;

    let hosts = get_easy_switch_hosts_on(&shared).await.expect("the read");

    assert_eq!(hosts.current, None);
    assert_eq!(hosts.slots.len(), 3);
}

/// Reads whose replies answer another request: the name of the next slot,
/// the next chunk of the name, and the OS of the next slot.
#[tokio::test]
async fn a_reply_for_another_slot_or_offset_fails_the_read() {
    let keyboards = [
        Keyboard {
            echoes_next_slot: Some(0x03),
            ..Keyboard::new()
        },
        Keyboard {
            echoes_next_offset: true,
            ..Keyboard::new()
        },
        Keyboard {
            echoes_next_slot: Some(0x07),
            ..Keyboard::new()
        },
    ];
    for keyboard in keyboards {
        let keyboard = Arc::new(Mutex::new(keyboard));
        let shared = keyboard_channel(&keyboard).await;
        assert_matches!(
            get_easy_switch_hosts_on(&shared).await,
            Err(WriteError::UnsupportedResponse {
                operation: HidppOperation::ReadHosts,
                feature_hex: 0x1815,
            })
        );
    }
}
