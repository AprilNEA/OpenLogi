//! Unit tests for `TouchpadRawXy` info parsing and raw-event decoding.

use std::assert_matches;

use super::event::{TouchpadRawEvent, decode_event};
use super::{Origin, RawReportFlags, TouchpadInfo};

#[test]
fn parses_touchpad_info() {
    let mut payload = [0; 16];
    payload[0..2].copy_from_slice(&1920u16.to_be_bytes());
    payload[2..4].copy_from_slice(&1080u16.to_be_bytes());
    payload[4] = 0x0f; // z range 16-bit
    payload[5] = 0x0f; // area range 16-bit
    payload[6] = 1; // 0.1 ms timestamp units
    payload[7] = 5; // max fingers
    payload[8] = 1; // origin = lower-left
    payload[9] = 1; // pen support
    payload[12] = 2; // mapping version
    payload[13..15].copy_from_slice(&1200u16.to_be_bytes());

    let info = TouchpadInfo::from_payload(&payload).unwrap();
    assert_eq!(info.x_size, 1920);
    assert_eq!(info.y_size, 1080);
    assert_eq!(info.z_data_range, 0x0f);
    assert_eq!(info.timestamp_units, 1);
    assert_eq!(info.max_finger_count, 5);
    assert_eq!(info.origin, Origin::LowerLeft);
    assert!(info.pen_support);
    assert_eq!(info.raw_report_mapping_version, 2);
    assert_eq!(info.dpi, 1200);
}

#[test]
fn rejects_reserved_origin() {
    let mut payload = [0; 16];
    payload[8] = 0; // 0x00 is reserved

    assert_matches!(
        TouchpadInfo::from_payload(&payload),
        Err(crate::protocol::v20::Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn decodes_dual_xy_event() {
    let mut payload = [0; 16];
    payload[0..2].copy_from_slice(&0x1234u16.to_be_bytes()); // timestamp
    // touch 1: CPT=0, X=0x0123; CTS=1, Y=0x0045; z=0x10, area=0x20.
    payload[2] = 0x01; // CPT1(0) | X1[13:8]=0x01
    payload[3] = 0x23; // X1[7:0]
    payload[4] = 0x40; // CTS1(1) | Y1[13:8]=0x00
    payload[5] = 0x45; // Y1[7:0]
    payload[6] = 0x10; // z1
    payload[7] = 0x20; // area1
    payload[8] = 0x30 | 0x04; // FID1=3, BTN set (bit2), EOF clear
    // touch 2: CPT=2, X=0x0210; CTS=0, Y=0x0305; z=0x11, area=0x22.
    payload[9] = 0x80 | 0x02; // CPT2(2) | X2[13:8]=0x02
    payload[10] = 0x10; // X2[7:0]
    payload[11] = 0x03; // CTS2(0) | Y2[13:8]=0x03
    payload[12] = 0x05; // Y2[7:0]
    payload[13] = 0x11; // z2
    payload[14] = 0x22; // area2
    payload[15] = 0x50 | 0x03; // FID2=5, NUMFING=3

    let TouchpadRawEvent::DualXy(frame) = decode_event(0, &payload).unwrap();
    assert_eq!(frame.timestamp, 0x1234);

    assert_eq!(frame.touch1.contact_type, 0);
    assert_eq!(frame.touch1.x, 0x0123);
    assert_eq!(frame.touch1.contact_status, 1);
    assert_eq!(frame.touch1.y, 0x0045);
    assert_eq!(frame.touch1.z, 0x10);
    assert_eq!(frame.touch1.area, 0x20);
    assert_eq!(frame.touch1.finger_id, 3);

    assert_eq!(frame.touch2.contact_type, 2);
    assert_eq!(frame.touch2.x, 0x0210);
    assert_eq!(frame.touch2.contact_status, 0);
    assert_eq!(frame.touch2.y, 0x0305);
    assert_eq!(frame.touch2.finger_id, 5);

    assert!(frame.button);
    assert!(!frame.end_of_frame);
    assert_eq!(frame.finger_count, 3);
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(1, &[0; 16]).is_none());
}

#[test]
fn raw_report_flag_bits() {
    let flags = RawReportFlags::from_bits_retain(0x05);
    assert!(flags.contains(RawReportFlags::RAW));
    assert!(flags.contains(RawReportFlags::ENHANCED));
    assert!(!flags.contains(RawReportFlags::FORCE_ADD));
}
