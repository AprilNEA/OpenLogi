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

#[test]
fn a_malformed_version_is_still_refused() {
    // Only the entity type became lenient: a non-BCD firmware number fails.
    let mut payload = fw_info(10);
    payload[4] = 0x1a;
    DeviceEntityFirmwareInfo::from_payload(&payload).unwrap_err();
}
