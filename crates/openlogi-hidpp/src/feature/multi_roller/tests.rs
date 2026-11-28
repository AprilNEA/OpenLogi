//! Unit tests for `MultiRoller` decoding, pinned to frames captured from an
//! MX Creative Dialpad (`docs/hardware/mx-creative-console.md`).

use std::assert_matches;

use super::{MultiRollerEvent, RollerId, RollerReportingMode, RollerRotation, decode_mode};
use crate::feature::DecodeEvent as _;

/// The 16-byte HID++ payload of a captured long report (`11 ff 0d 00 …`).
fn payload(bytes: &[u8]) -> [u8; 16] {
    let mut p = [0; 16];
    p[..bytes.len()].copy_from_slice(bytes);
    p
}

#[test]
fn decodes_a_counter_clockwise_dial_step() {
    // 11 ff 0d 00 01 ff 00 32 7e 97
    let event = MultiRollerEvent::decode(0, &payload(&[0x01, 0xff, 0x00, 0x32, 0x7e, 0x97]));
    assert_eq!(
        event,
        Some(MultiRollerEvent::Rotation(RollerRotation {
            roller: RollerId(1),
            delta: -1,
            timestamp_ms: 0x0032_7e97,
        }))
    );
}

#[test]
fn decodes_a_fast_roller_burst() {
    // 11 ff 0d 00 00 04 00 32 da a3
    let Some(MultiRollerEvent::Rotation(rotation)) =
        MultiRollerEvent::decode(0, &payload(&[0x00, 0x04, 0x00, 0x32, 0xda, 0xa3]))
    else {
        panic!("rotation not decoded");
    };
    assert_eq!(rotation.roller, RollerId(0));
    assert_eq!(rotation.delta, 4);
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(MultiRollerEvent::decode(1, &[0; 16]).is_none());
}

#[test]
fn decodes_both_modes_and_rejects_unknown_bytes() {
    assert_eq!(decode_mode(0).ok(), Some(RollerReportingMode::Native));
    assert_eq!(decode_mode(1).ok(), Some(RollerReportingMode::Diverted));
    assert_matches!(
        decode_mode(2),
        Err(crate::protocol::v20::Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn capabilities_enumerate_every_roller() {
    let caps = super::MultiRollerCapabilities { roller_count: 2 };
    assert_eq!(
        caps.rollers().collect::<Vec<_>>(),
        [RollerId(0), RollerId(1)]
    );
}
