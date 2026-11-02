//! Tests for the bounded `0x2205` pointer-speed reads and writes.

use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

/// The `0x2205` feature index on the scripted mouse.
const SCALING_INDEX: u8 = 0x0d;
/// HID++ 2.0 `Busy`.
const BUSY: u8 = 0x08;

/// A mouse whose firmware stores every scaling it is sent, unclipped, as the
/// MX Ergo does (#1372).
struct Mouse {
    scaling: u16,
    /// Whether a read after a write fails with `Busy`.
    fails_read_back: bool,
    /// Every scaling written.
    writes: Vec<u16>,
}

impl Mouse {
    fn at(scaling: u16) -> Self {
        Self {
            scaling,
            fails_read_back: false,
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
                if u16::from_be_bytes([request[4], request[5]]) == 0x2205 {
                    payload[0] = SCALING_INDEX;
                }
            }
            (SCALING_INDEX, 0x00) => {
                if self.fails_read_back && !self.writes.is_empty() {
                    return Some(feature_error(request, BUSY));
                }
                payload[..2].copy_from_slice(&self.scaling.to_be_bytes());
            }
            (SCALING_INDEX, 0x01) => {
                let scaling = u16::from_be_bytes([request[4], request[5]]);
                self.writes.push(scaling);
                self.scaling = scaling;
                payload[..2].copy_from_slice(&scaling.to_be_bytes());
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

async fn mouse_channel(mouse: &Arc<Mutex<Mouse>>) -> SharedChannel {
    let mouse = Arc::clone(mouse);
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        mouse.lock().expect("mouse lock").respond(request)
    });
    let route = DeviceRoute::Unifying {
        receiver_uid: "UNIFYING".to_string(),
        slot: 1,
    };
    SharedChannel::new(scripted_channel(raw).await, route)
}

fn speed(raw: u16) -> PointerSpeed {
    PointerSpeed::try_new(raw).expect("a speed in range")
}

#[tokio::test]
async fn reads_the_speed_the_mouse_moves_at() {
    let mouse = Arc::new(Mutex::new(Mouse::at(0x0180)));
    let shared = mouse_channel(&mouse).await;

    let read = get_pointer_speed_on(&shared)
        .await
        .expect("the mouse answers");

    assert_eq!(read, speed(0x0180));
}

#[tokio::test]
async fn an_unclipped_reading_comes_back_inside_the_range() {
    // MX Ergo firmware keeps 0x0001 as written; the pointer barely moves.
    let mouse = Arc::new(Mutex::new(Mouse::at(0x0001)));
    let shared = mouse_channel(&mouse).await;

    let read = get_pointer_speed_on(&shared)
        .await
        .expect("the mouse answers");

    assert_eq!(read, PointerSpeed::MIN);
}

#[tokio::test]
async fn writes_the_speed_and_returns_the_read_back() {
    let mouse = Arc::new(Mutex::new(Mouse::at(0x0100)));
    let shared = mouse_channel(&mouse).await;

    let applied = set_pointer_speed_on(&shared, PointerSpeed::MIN)
        .await
        .expect("the mouse takes the write");

    assert_eq!(applied, PointerSpeed::MIN);
    assert_eq!(mouse.lock().expect("mouse lock").writes, [0x0020]);
}

#[tokio::test]
async fn a_failed_read_back_still_reports_the_acknowledged_write() {
    let mouse = Arc::new(Mutex::new(Mouse {
        fails_read_back: true,
        ..Mouse::at(0x0100)
    }));
    let shared = mouse_channel(&mouse).await;

    let applied = set_pointer_speed_on(&shared, PointerSpeed::MAX)
        .await
        .expect("the write was acknowledged");

    assert_eq!(applied, PointerSpeed::MAX);
    assert_eq!(mouse.lock().expect("mouse lock").scaling, 0x0300);
}
