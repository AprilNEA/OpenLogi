//! Unit tests for `TouchMouseRaw` info parsing and raw-event decoding.

use super::event::{TouchMousePoint, TouchMouseRawEvent, TouchMouseStatus, decode_event};
use super::{Origin, RawMode, TouchMouseInfo};

#[test]
fn parses_touch_mouse_info() {
    let mut payload = [0; 16];
    payload[0..2].copy_from_slice(&2048u16.to_be_bytes());
    payload[2..4].copy_from_slice(&1280u16.to_be_bytes());
    payload[4..6].copy_from_slice(&1200u16.to_be_bytes());
    payload[6] = 3; // upper-left
    payload[7] = 4; // max fingers
    payload[8] = 15; // width/height data range

    let info = TouchMouseInfo::from_payload(&payload).unwrap();
    assert_eq!(info.x_max_count, 2048);
    assert_eq!(info.y_max_count, 1280);
    assert_eq!(info.resolution_dpi, 1200);
    assert_eq!(info.origin, Origin::UpperLeft);
    assert_eq!(info.max_finger_count, 4);
    assert_eq!(info.width_height_data_range, 15);
}

#[test]
fn decodes_raw_data_with_lifted_fingers() {
    // touch 0 present (X=0x123, Y=0x045, Wx=2, Wy=3); touch 2 present (X=0x200,
    // Y=0x100, Wx=1, Wy=1); touches 1 and 3 lifted (0xFF).
    let payload = [
        0x12, 0x04, 0x53, 0x32, // touch 0
        0xff, 0xff, 0xff, 0xff, // touch 1 lifted
        0x20, 0x10, 0x00, 0x11, // touch 2
        0xff, 0xff, 0xff, 0xff, // touch 3 lifted
    ];

    let TouchMouseRawEvent::RawData { touches } = decode_event(0, &payload).unwrap() else {
        panic!("expected raw data");
    };
    assert_eq!(
        touches[0],
        Some(TouchMousePoint {
            x: 0x123,
            y: 0x045,
            width_x: 2,
            width_y: 3
        })
    );
    assert_eq!(touches[1], None);
    assert_eq!(
        touches[2],
        Some(TouchMousePoint {
            x: 0x200,
            y: 0x100,
            width_x: 1,
            width_y: 1
        })
    );
    assert_eq!(touches[3], None);
}

#[test]
fn decodes_status_event() {
    let mut payload = [0; 16];
    payload[0] = 0b11; // mouse lifted + button down

    let TouchMouseRawEvent::StatusChanged(status) = decode_event(1, &payload).unwrap() else {
        panic!("expected status event");
    };
    assert!(status.contains(TouchMouseStatus::MOUSE_LIFTED));
    assert!(status.contains(TouchMouseStatus::BUTTON_DOWN));
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(2, &[0; 16]).is_none());
}

#[test]
fn maps_raw_mode_wire_values() {
    assert_eq!(RawMode::try_from(0u8).unwrap(), RawMode::NativeGestures);
    assert_eq!(RawMode::try_from(4u8).unwrap(), RawMode::RawUnfilteredWithZ);
    assert!(RawMode::try_from(5u8).is_err());
    assert_eq!(u8::from(RawMode::RawFiltered), 1);
}
