//! Tests for the `0x8300` sidetone reads and writes.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

/// The `0x8300` feature index on a G535.
const SIDETONE_INDEX: u8 = 0x04;
/// HID++ 2.0 `InvalidArgument`, the G535's answer to a level above 100.
const INVALID_ARGUMENT: u8 = 0x02;

/// A G535 as measured: it echoes each write, but a read just after one
/// still answers the level before it, and anything above 100 is refused.
struct Headset {
    /// What a read answers: the level before the newest write.
    reads: u8,
    /// The level the newest accepted write set.
    level: u8,
    /// Whether the table lists `0x8300` at all.
    has_sidetone: bool,
    /// Every level written, accepted or not.
    writes: Vec<u8>,
}

impl Headset {
    fn g535(level: u8) -> Self {
        Self {
            reads: level,
            level,
            has_sidetone: true,
            writes: Vec::new(),
        }
    }

    fn respond(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
            return None;
        }
        let function = request[3] >> 4;
        let mut payload = [0u8; 16];
        match (request[2], function) {
            (0x00, 0x01) => payload[0] = 4,
            (0x00, 0x00) => {
                if self.has_sidetone && u16::from_be_bytes([request[4], request[5]]) == 0x8300 {
                    payload[0] = SIDETONE_INDEX;
                }
            }
            (SIDETONE_INDEX, 0x00) => payload[0] = self.reads,
            (SIDETONE_INDEX, 0x01) => {
                let level = request[4];
                self.writes.push(level);
                if level > 100 {
                    return Some(feature_error(request, INVALID_ARGUMENT));
                }
                self.reads = self.level;
                self.level = level;
                payload[0] = level;
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

async fn headset_channel(headset: &Arc<Mutex<Headset>>) -> SharedChannel {
    let headset = Arc::clone(headset);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        headset.lock().expect("headset lock").respond(request)
    });
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0x0ac4,
    };
    SharedChannel::new(scripted_channel(raw).await, route)
}

fn level(raw: u8) -> SidetoneLevel {
    SidetoneLevel::try_new(raw).expect("a level in range")
}

#[tokio::test]
async fn reads_the_level_the_headset_reports() {
    let headset = Arc::new(Mutex::new(Headset::g535(40)));
    let shared = headset_channel(&headset).await;

    assert_eq!(get_sidetone_on(&shared).await.unwrap(), level(40));
    assert!(
        headset.lock().unwrap().writes.is_empty(),
        "a read writes nothing"
    );
}

#[tokio::test]
async fn a_write_returns_the_echo_not_the_stale_read() {
    let headset = Arc::new(Mutex::new(Headset::g535(0)));
    let shared = headset_channel(&headset).await;

    assert_eq!(
        set_sidetone_on(&shared, level(30)).await.unwrap(),
        level(30)
    );
    assert_eq!(
        get_sidetone_on(&shared).await.unwrap(),
        level(0),
        "the scripted G535 still reads the old level, as the hardware does"
    );
    assert_eq!(headset.lock().unwrap().writes, [30]);
}

#[tokio::test]
async fn a_headset_without_the_feature_is_unsupported() {
    let headset = Arc::new(Mutex::new(Headset {
        has_sidetone: false,
        ..Headset::g535(0)
    }));
    let shared = headset_channel(&headset).await;

    assert_matches!(
        get_sidetone_on(&shared).await,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x8300
        })
    );
}
