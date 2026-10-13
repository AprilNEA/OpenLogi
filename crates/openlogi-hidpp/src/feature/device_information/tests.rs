//! Unit tests for `DeviceInformation` firmware-entity parsing.

use super::{DeviceEntityFirmwareInfo, DeviceEntityType};

/// A getFwInfo reply for an entity of type `kind`: `MPM17.00_B0008`, active,
/// transport PID `0xc08d`.
fn fw_info(kind: u8) -> [u8; 16] {
    let mut payload = [0; 16];
    payload[0] = kind;
    payload[1..4].copy_from_slice(b"MPM");
    payload[4] = 0x17; // firmware number, packed BCD
    payload[5] = 0x00; // revision
    payload[6..8].copy_from_slice(&[0x00, 0x08]); // build
    payload[8] = 1; // active
    payload[9..11].copy_from_slice(&0xc08du16.to_be_bytes());
    payload
}

#[test]
fn decodes_named_entity_types() {
    for (byte, kind) in [
        (0, DeviceEntityType::MainApplication),
        (1, DeviceEntityType::Bootloader),
        (2, DeviceEntityType::Hardware),
        (9, DeviceEntityType::MotorDrive),
    ] {
        let info = DeviceEntityFirmwareInfo::from_payload(&fw_info(byte)).unwrap();
        assert_eq!(info.entity_type, kind);
        assert_eq!(u8::from(kind), byte);
    }
}

#[test]
fn an_unnamed_entity_type_still_decodes() {
    // The draft spec groups 3 to 15 as "other"; 10 to 15 have no name here
    // and used to fail the whole entity with UnsupportedResponse.
    for byte in [10, 15, 0x80] {
        let info = DeviceEntityFirmwareInfo::from_payload(&fw_info(byte)).unwrap();
        assert_eq!(info.entity_type, DeviceEntityType::Other(byte));
        assert_eq!(u8::from(info.entity_type), byte);
        assert_eq!(info.firmware_prefix, "MPM");
        assert_eq!(
            (info.firmware_number, info.revision, info.build),
            (17, 0, 8)
        );
        assert!(info.active);
        assert_eq!(info.transport_pid, 0xc08d);
    }
}

/// Decodes `fw_info(0)` with the version and build bytes replaced.
fn version_and_build(version: [u8; 2], build: [u8; 2]) -> (u8, u8, u16) {
    let mut payload = fw_info(0);
    payload[4..6].copy_from_slice(&version);
    payload[6..8].copy_from_slice(&build);
    let info = DeviceEntityFirmwareInfo::from_payload(&payload).unwrap();
    (info.firmware_number, info.revision, info.build)
}

#[test]
fn a_bcd_version_decodes_digit_by_digit() {
    assert_eq!(version_and_build([0x92, 0x00], [0x00, 0x08]), (92, 0, 8));
    assert_eq!(
        version_and_build([0x99, 0x99], [0x99, 0x99]),
        (99, 99, 9999)
    );
}

#[test]
fn a_binary_build_is_read_as_a_number() {
    // A G502 LIGHTSPEED's Softdevice, byte for byte as captured in #690:
    // build 0x00a9 is not BCD. Options+ reads it as 169, and so does this
    // parser now instead of refusing the entity.
    let mut payload = [0; 16];
    payload[..8].copy_from_slice(&[0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xa9]);
    let info = DeviceEntityFirmwareInfo::from_payload(&payload).unwrap();
    assert_eq!(info.entity_type, DeviceEntityType::Softdevice);
    assert_eq!(info.firmware_prefix, "\0\0\0");
    assert_eq!(
        (info.firmware_number, info.revision, info.build),
        (0, 0, 169)
    );
    assert!(!info.active);
}

#[test]
fn a_binary_version_is_split_into_two_digit_halves() {
    // The version is one field: 0x010a is 266, which reads as 02.66, not as
    // a valid number byte 01 beside a broken revision byte.
    assert_eq!(version_and_build([0x01, 0x0a], [0x00, 0x08]), (2, 66, 8));
    // One bad nibble in the low byte is enough to switch the whole field.
    assert_eq!(version_and_build([0x17, 0x0a], [0x00, 0x08]), (58, 98, 8));
}

#[test]
fn a_binary_field_is_capped_at_four_digits() {
    assert_eq!(
        version_and_build([0xff, 0xff], [0xff, 0xff]),
        (99, 99, 9999)
    );
    // Around the cap, with a non-decimal nibble so the field is binary:
    // 0x26ff is 9983, 0x270f is exactly 9999, 0x271a is 10010.
    assert_eq!(version_and_build([0x00, 0x00], [0x26, 0xff]).2, 9983);
    assert_eq!(version_and_build([0x00, 0x00], [0x27, 0x0f]).2, 9999);
    assert_eq!(version_and_build([0x00, 0x00], [0x27, 0x1a]).2, 9999);
}

#[test]
fn a_prefix_that_is_not_utf8_is_still_refused() {
    let mut payload = fw_info(0);
    payload[1..4].copy_from_slice(&[0xff, 0xfe, 0xfd]);
    DeviceEntityFirmwareInfo::from_payload(&payload).unwrap_err();
}
