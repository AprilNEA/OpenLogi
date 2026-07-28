//! Unit tests for `Crown` mode parsing and event decoding.

use std::assert_matches;

use super::event::{
    ActivityState, ButtonState, CrownEvent, CrownGesture, RotationState, decode_event,
};
use super::{CrownMode, RatchetMode, ReportingMode};

#[test]
fn parses_mode() {
    let mut payload = [0; 16];
    payload[0] = 2; // Diverted
    payload[1] = 2; // Ratchet
    payload[2] = 0x10;
    payload[3] = 0x20;
    payload[4] = 0x05;

    let mode = CrownMode::from_payload(&payload).unwrap();
    assert_eq!(mode.diverting, ReportingMode::Diverted);
    assert_eq!(mode.ratchet_mode, RatchetMode::Ratchet);
    assert_eq!(mode.rotation_timeout, 0x10);
    assert_eq!(mode.short_long_timeout, 0x20);
    assert_eq!(mode.double_tap_speed, 0x05);
}

#[test]
fn rejects_unknown_mode_value() {
    let mut payload = [0; 16];
    payload[0] = 9;

    assert_matches!(
        CrownMode::from_payload(&payload),
        Err(crate::protocol::v20::Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn decodes_crown_event_with_signed_fields() {
    let mut payload = [0; 16];
    payload[0] = 1; // Start
    payload[1] = 0xfb; // -5 slots
    payload[2] = 0x03; // +3 ratchets
    payload[3] = 2; // proximity Active
    payload[4] = 1; // touch Start
    payload[5] = 1; // Tap
    payload[6] = 3; // LongPress
    payload[14..16].copy_from_slice(&(-200i16).to_be_bytes());

    let CrownEvent::Update(update) = decode_event(0, &payload).unwrap();
    assert_eq!(update.rotation_state, RotationState::Start);
    assert_eq!(update.relative_slot_rotation, -5);
    assert_eq!(update.relative_ratchet_rotation, 3);
    assert_eq!(update.proximity, ActivityState::Active);
    assert_eq!(update.touch, ActivityState::Start);
    assert_eq!(update.gesture, CrownGesture::Tap);
    assert_eq!(update.button, ButtonState::LongPress);
    assert_eq!(update.speed, -200);
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(1, &[0; 16]).is_none());
}

#[test]
fn ignores_event_with_unknown_enum() {
    let mut payload = [0; 16];
    payload[6] = 0x09; // out-of-range button state
    assert!(decode_event(0, &payload).is_none());
}

#[test]
fn maps_mode_enum_wire_values() {
    assert_eq!(u8::from(ReportingMode::Diverted), 2);
    assert_eq!(ReportingMode::try_from(1u8).unwrap(), ReportingMode::Hid);
    assert_eq!(u8::from(RatchetMode::Free), 1);
    assert!(RatchetMode::try_from(3u8).is_err());
}
