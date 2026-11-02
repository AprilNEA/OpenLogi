//! Tests for the `0x1300` DPI LED write.

use std::sync::{Arc, Mutex};

use super::*;
use crate::channel::route::DeviceRoute;
use crate::channel::scripted::{ScriptedRawHidChannel, scripted_channel};

/// The `0x1300` feature index on the scripted mouse.
const LED_INDEX: u8 = 0x0e;

/// `GetLEDInfo` LED types.
const BATTERY: u8 = 1;
const DPI: u8 = 2;

/// A mouse with `0x1300` LEDs of the types in `leds`, by index.
struct Mouse {
    /// Whether the mouse lists `0x1300` at all.
    has_feature: bool,
    leds: Vec<u8>,
    /// Whether software drives the LEDs.
    software: bool,
    /// Every `SetSWControl` flag sent.
    control_writes: Vec<u8>,
    /// Every `SetLEDState` sent, as LED index and state.
    state_writes: Vec<(u8, u16)>,
}

impl Mouse {
    fn with_leds(leds: &[u8]) -> Self {
        Self {
            has_feature: true,
            leds: leds.to_vec(),
            software: false,
            control_writes: Vec::new(),
            state_writes: Vec::new(),
        }
    }

    fn without_feature() -> Self {
        Self {
            has_feature: false,
            ..Self::with_leds(&[])
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
                if self.has_feature && u16::from_be_bytes([request[4], request[5]]) == 0x1300 {
                    payload[0] = LED_INDEX;
                }
            }
            (LED_INDEX, 0x00) => {
                payload[0] = u8::try_from(self.leds.len()).expect("a few LEDs");
            }
            (LED_INDEX, 0x01) => {
                let led = request[4];
                payload[0] = led;
                payload[1] = self.leds[usize::from(led)];
                payload[2] = 1;
                payload[3..5].copy_from_slice(&0x0003u16.to_be_bytes());
            }
            (LED_INDEX, 0x02) => payload[0] = u8::from(self.software),
            (LED_INDEX, 0x03) => {
                self.control_writes.push(request[4]);
                self.software = request[4] != 0;
            }
            (LED_INDEX, 0x05) => {
                self.state_writes
                    .push((request[4], u16::from_be_bytes([request[5], request[6]])));
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

#[tokio::test]
async fn lights_the_first_dpi_led_after_taking_software_control() {
    let mouse = Arc::new(Mutex::new(Mouse::with_leds(&[BATTERY, DPI, DPI])));
    let shared = mouse_channel(&mouse).await;

    let led = set_dpi_led_on(&shared, true)
        .await
        .expect("the mouse answers");

    assert_eq!(led, DpiLed::Set);
    let mouse = mouse.lock().expect("mouse lock");
    assert_eq!(mouse.control_writes, [1]);
    assert_eq!(mouse.state_writes, [(1, 0x0002)]);
}

#[tokio::test]
async fn darkens_the_dpi_led_without_asking_for_control_it_has() {
    let mouse = Arc::new(Mutex::new(Mouse::with_leds(&[DPI])));
    mouse.lock().expect("mouse lock").software = true;
    let shared = mouse_channel(&mouse).await;

    let led = set_dpi_led_on(&shared, false)
        .await
        .expect("the mouse answers");

    assert_eq!(led, DpiLed::Set);
    let mouse = mouse.lock().expect("mouse lock");
    assert!(mouse.control_writes.is_empty());
    assert_eq!(mouse.state_writes, [(0, 0x0001)]);
}

#[tokio::test]
async fn a_mouse_without_a_dpi_led_gets_no_write() {
    let mouse = Arc::new(Mutex::new(Mouse::with_leds(&[BATTERY])));
    let shared = mouse_channel(&mouse).await;

    let led = set_dpi_led_on(&shared, true)
        .await
        .expect("the mouse answers");

    assert_eq!(led, DpiLed::Missing);
    let mouse = mouse.lock().expect("mouse lock");
    assert!(mouse.control_writes.is_empty());
    assert!(mouse.state_writes.is_empty());
}

#[tokio::test]
async fn a_mouse_without_led_control_is_missing_the_led() {
    let mouse = Arc::new(Mutex::new(Mouse::without_feature()));
    let shared = mouse_channel(&mouse).await;

    let led = set_dpi_led_on(&shared, true)
        .await
        .expect("the mouse answers");

    assert_eq!(led, DpiLed::Missing);
}
