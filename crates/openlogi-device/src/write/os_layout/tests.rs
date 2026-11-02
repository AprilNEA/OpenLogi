//! Tests for keeping a keyboard on the host OS's `0x4531` platform.

use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

const MACOS: HostOs = HostOs {
    os: OsMask::MACOS,
    version: Some(OsVersion {
        major: 15,
        revision: 0,
    }),
};

/// A descriptor for `os` on platform `platform`, ranged `from..=to` as
/// `[major, revision]` (`[0, 0]` leaves that end open).
fn descriptor(platform: u8, descriptor: u8, os: OsMask, from: [u8; 2], to: [u8; 2]) -> Candidate {
    Candidate {
        platform,
        descriptor,
        os,
        from: u16::from_be_bytes(from),
        to: u16::from_be_bytes(to),
    }
}

#[test]
fn picks_the_platform_whose_descriptor_names_the_host_os() {
    let descriptors = [
        descriptor(0, 0, OsMask::WINDOWS, [0, 0], [0, 0]),
        descriptor(1, 1, OsMask::MACOS, [0, 0], [0, 0]),
        descriptor(2, 2, OsMask::IOS, [0, 0], [0, 0]),
    ];
    assert_eq!(select_platform(descriptors, MACOS), Some(1));
    let windows = HostOs {
        os: OsMask::WINDOWS,
        version: None,
    };
    assert_eq!(select_platform(descriptors, windows), Some(0));
    let linux = HostOs {
        os: OsMask::LINUX,
        version: None,
    };
    assert_eq!(select_platform(descriptors, linux), None);
}

#[test]
fn a_version_outside_the_range_does_not_match() {
    let descriptors = [
        descriptor(0, 0, OsMask::MACOS, [10, 0], [10, 15]),
        descriptor(1, 1, OsMask::MACOS, [11, 0], [0, 0]),
    ];
    assert_eq!(select_platform(descriptors, MACOS), Some(1));
    let catalina = HostOs {
        os: OsMask::MACOS,
        version: Some(OsVersion {
            major: 10,
            revision: 15,
        }),
    };
    assert_eq!(select_platform(descriptors, catalina), Some(0));
}

#[test]
fn ranks_matches_the_way_options_plus_does() {
    // A bounded range that covers the host ranks before an open-ended one.
    let descriptors = [
        descriptor(0, 0, OsMask::MACOS, [0, 0], [0, 0]),
        descriptor(1, 1, OsMask::MACOS, [11, 0], [26, 0]),
    ];
    assert_eq!(select_platform(descriptors, MACOS), Some(1));
    // The higher upper bound wins, then the lower lower bound.
    let descriptors = [
        descriptor(0, 0, OsMask::MACOS, [0, 0], [16, 0]),
        descriptor(1, 1, OsMask::MACOS, [0, 0], [20, 0]),
        descriptor(2, 2, OsMask::MACOS, [12, 0], [20, 0]),
    ];
    assert_eq!(select_platform(descriptors, MACOS), Some(1));
    // A full tie goes to the first platform.
    let descriptors = [
        descriptor(2, 0, OsMask::MACOS, [0, 0], [0, 0]),
        descriptor(1, 1, OsMask::MACOS, [0, 0], [0, 0]),
    ];
    assert_eq!(select_platform(descriptors, MACOS), Some(1));
}

#[test]
fn an_unknown_version_matches_only_an_open_ended_range() {
    let unknown = HostOs {
        os: OsMask::MACOS,
        version: None,
    };
    let descriptors = [
        descriptor(0, 0, OsMask::MACOS, [10, 0], [10, 15]),
        descriptor(1, 1, OsMask::MACOS, [11, 0], [0, 0]),
    ];
    assert_eq!(select_platform(descriptors, unknown), Some(1));
    let bounded_only = [descriptor(0, 0, OsMask::MACOS, [10, 0], [10, 15])];
    assert_eq!(select_platform(bounded_only, unknown), None);
}

/// The `0x4531` feature index on the scripted keyboard.
const FEATURE_INDEX: u8 = 0x0e;

/// A three-slot keyboard with Windows (0), macOS (1) and iOS (2) platforms.
struct Keyboard {
    capabilities: u16,
    platform_count: u8,
    host_count: u8,
    current_host: u8,
    descriptors: Vec<[u8; 8]>,
    platforms: [u8; 3],
    /// Whether a `setHostPlatform` write changes the slot.
    takes_writes: bool,
    /// Every `setHostPlatform` request, as `[host, platform, reserved]`.
    writes: Vec<[u8; 3]>,
    /// How many requests reached the keyboard.
    requests: usize,
}

impl Keyboard {
    fn new() -> Self {
        Self {
            capabilities: MultiPlatformCapabilities::SET_HOST_PLATFORM.bits(),
            platform_count: 3,
            host_count: 3,
            current_host: 1,
            descriptors: vec![
                [0, 0, 0x01, 0x00, 0, 0, 0, 0],
                [1, 1, 0x20, 0x00, 0, 0, 0, 0],
                [2, 2, 0x40, 0x00, 0, 0, 0, 0],
            ],
            platforms: [0; 3],
            takes_writes: true,
            writes: Vec::new(),
            requests: 0,
        }
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        self.requests += 1;
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let function = request[3] >> 4;
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                let feature_id = u16::from_be_bytes([request[4], request[5]]);
                payload[0] = u8::from(feature_id == 0x4531) * FEATURE_INDEX;
            }
            (FEATURE_INDEX, 0x00) => {
                payload[..2].copy_from_slice(&self.capabilities.to_be_bytes());
                payload[2] = self.platform_count;
                payload[3] = u8::try_from(self.descriptors.len()).ok()?;
                payload[4] = self.host_count;
                payload[5] = self.current_host;
                payload[6] = *self.platforms.get(usize::from(self.current_host))?;
            }
            (FEATURE_INDEX, 0x01) => {
                payload[..8].copy_from_slice(self.descriptors.get(usize::from(request[4]))?);
            }
            (FEATURE_INDEX, 0x02) => {
                let host = request[4];
                payload[0] = host;
                payload[2] = *self.platforms.get(usize::from(host))?;
                payload[4] = 0xff;
                payload[5] = 0xff;
            }
            (FEATURE_INDEX, 0x03) => {
                let (host, platform) = (request[4], request[5]);
                self.writes.push([host, platform, request[6]]);
                if self.takes_writes {
                    *self.platforms.get_mut(usize::from(host))? = platform;
                }
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
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xb378,
        },
    )
}

#[tokio::test]
async fn switches_the_current_slot_to_the_host_platform_and_reads_it_back() {
    // Slot 2 and the macOS platform (1) differ, so the bytes cannot swap.
    let keyboard = Arc::new(Mutex::new(Keyboard {
        current_host: 2,
        ..Keyboard::new()
    }));
    let shared = keyboard_channel(&keyboard).await;

    let outcome = keep_os_layout_on(&shared, MACOS, || true)
        .await
        .expect("the keyboard takes the write");

    assert_eq!(
        outcome,
        OsLayoutOutcome::Switched {
            from: Some(0),
            to: 1
        }
    );
    let keyboard = keyboard.lock().expect("keyboard lock");
    // The slot is addressed explicitly, never as 0xFF; byte 2 is reserved.
    assert_eq!(keyboard.writes, [[2, 1, 0]]);
    assert_eq!(keyboard.platforms, [0, 0, 1]);
}

#[tokio::test]
async fn leaves_a_slot_already_on_the_host_platform_alone() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").platforms[1] = 1;
    let shared = keyboard_channel(&keyboard).await;

    let outcome = keep_os_layout_on(&shared, MACOS, || true)
        .await
        .expect("nothing to write");

    assert_eq!(outcome, OsLayoutOutcome::AlreadySet { platform_index: 1 });
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn does_not_write_a_keyboard_that_does_not_advertise_software_set() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").capabilities =
        MultiPlatformCapabilities::OS_DETECTION.bits();
    let shared = keyboard_channel(&keyboard).await;

    let outcome = keep_os_layout_on(&shared, MACOS, || true)
        .await
        .expect("no write");

    assert_eq!(outcome, OsLayoutOutcome::NotSettable);
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn does_not_write_a_slot_outside_the_host_count() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").host_count = 1;
    let shared = keyboard_channel(&keyboard).await;

    let outcome = keep_os_layout_on(&shared, MACOS, || true)
        .await
        .expect("no write");

    assert_eq!(outcome, OsLayoutOutcome::NotSettable);
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn does_not_write_when_no_descriptor_covers_the_host() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    let linux = HostOs {
        os: OsMask::LINUX,
        version: None,
    };

    let outcome = keep_os_layout_on(&shared, linux, || true)
        .await
        .expect("no write");

    assert_eq!(outcome, OsLayoutOutcome::NoMatchingPlatform);
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn does_not_write_a_platform_outside_the_platform_count() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").platform_count = 1;
    let shared = keyboard_channel(&keyboard).await;

    let outcome = keep_os_layout_on(&shared, MACOS, || true)
        .await
        .expect("no write");

    assert_eq!(outcome, OsLayoutOutcome::NoMatchingPlatform);
    assert!(keyboard.lock().expect("keyboard lock").writes.is_empty());
}

#[tokio::test]
async fn a_write_the_keyboard_does_not_read_back_is_an_error() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    keyboard.lock().expect("keyboard lock").takes_writes = false;
    let shared = keyboard_channel(&keyboard).await;

    let error = keep_os_layout_on(&shared, MACOS, || true)
        .await
        .expect_err("the slot still holds Windows");

    assert_eq!(
        error,
        WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteOsLayout,
            feature_hex: 0x4531,
        }
    );
    assert_eq!(keyboard.lock().expect("keyboard lock").writes, [[1, 1, 0]]);
}

#[tokio::test]
async fn a_write_withdrawn_before_it_starts_sends_nothing() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    let asked = std::sync::atomic::AtomicUsize::new(0);
    let outcome = keep_os_layout_on(&shared, MACOS, || {
        asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        false
    })
    .await
    .expect("nothing to write");
    assert_eq!(outcome, OsLayoutOutcome::Withdrawn);
    // Asked once, before any request.
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(keyboard.lock().expect("keyboard lock").requests, 0);
}

#[tokio::test]
async fn a_write_withdrawn_after_the_reads_is_not_sent() {
    let keyboard = Arc::new(Mutex::new(Keyboard::new()));
    let shared = keyboard_channel(&keyboard).await;
    // Wanted when the job starts, withdrawn by the time it would write.
    let asked = std::sync::atomic::AtomicUsize::new(0);
    let outcome = keep_os_layout_on(&shared, MACOS, || {
        asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0
    })
    .await
    .expect("nothing to write");
    assert_eq!(outcome, OsLayoutOutcome::Withdrawn);
    assert_eq!(asked.load(std::sync::atomic::Ordering::Relaxed), 2);
    let keyboard = keyboard.lock().expect("keyboard lock");
    assert!(keyboard.writes.is_empty());
    assert_eq!(keyboard.platforms, [0, 0, 0]);
}
