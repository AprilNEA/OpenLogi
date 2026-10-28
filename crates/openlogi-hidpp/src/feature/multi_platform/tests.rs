//! Unit tests for `MultiPlatform` reply and event parsing.

use super::{
    HostIndex, MultiPlatformCapabilities, MultiPlatformInfo, OsMask, PlatformChange,
    PlatformDescriptor, PlatformSource,
};

fn descriptor(mask: [u8; 2]) -> PlatformDescriptor {
    let mut payload = [0; 16];
    payload[0] = 2; // platform index
    payload[1] = 5; // descriptor index
    payload[2..4].copy_from_slice(&mask);
    payload[4..8].copy_from_slice(&[10, 15, 0, 0]);
    PlatformDescriptor::from_payload(&payload)
}

#[test]
fn reads_each_os_from_its_wire_bit() {
    // Mask bytes 2-3 as a device sends them, big-endian.
    for (bytes, os) in [
        ([0x00, 0x01], OsMask::TIZEN),
        ([0x01, 0x00], OsMask::WINDOWS),
        ([0x02, 0x00], OsMask::WINDOWS_EMBEDDED),
        ([0x04, 0x00], OsMask::LINUX),
        ([0x08, 0x00], OsMask::CHROME),
        ([0x10, 0x00], OsMask::ANDROID),
        ([0x20, 0x00], OsMask::MACOS),
        ([0x40, 0x00], OsMask::IOS),
        ([0x80, 0x00], OsMask::WEBOS),
    ] {
        assert_eq!(descriptor(bytes).os_mask, os, "mask bytes {bytes:02x?}");
    }
}

#[test]
fn a_windows_descriptor_is_not_tizen() {
    // 0x0100 used to decode as Tizen, and a macOS row (0x2000) as no OS at all.
    let windows = descriptor([0x01, 0x00]);
    assert!(windows.os_mask.contains(OsMask::WINDOWS));
    assert!(!windows.os_mask.contains(OsMask::TIZEN));

    let apple = descriptor([0x60, 0x00]);
    assert_eq!(apple.os_mask, OsMask::MACOS | OsMask::IOS);
}

#[test]
fn decodes_the_rest_of_the_descriptor() {
    let row = descriptor([0x01, 0x00]);
    assert_eq!(row.platform_index, 2);
    assert_eq!(row.descriptor_index, 5);
    assert_eq!(
        (
            row.from_version,
            row.from_revision,
            row.to_version,
            row.to_revision
        ),
        (10, 15, 0, 0)
    );
}

#[test]
fn keeps_undefined_bits() {
    // Bits 1-7 are undefined; they survive decoding instead of being dropped.
    assert_eq!(descriptor([0x01, 0x82]).os_mask.bits(), 0x0182);
}

#[test]
fn set_host_platform_is_bit_one_of_byte_zero() {
    // Options+ 2.8 reads the capability word big-endian and allows a
    // software platform write only while 0x0200 is set.
    let mut payload = [0; 16];
    payload[0] = 0x02;
    let info = MultiPlatformInfo::from_payload(&payload);
    assert_eq!(
        info.capabilities,
        MultiPlatformCapabilities::SET_HOST_PLATFORM
    );
    assert_eq!(MultiPlatformCapabilities::SET_HOST_PLATFORM.bits(), 0x0200);

    let mut payload = [0; 16];
    payload[1] = 0x02;
    let info = MultiPlatformInfo::from_payload(&payload);
    assert!(
        !info
            .capabilities
            .contains(MultiPlatformCapabilities::SET_HOST_PLATFORM)
    );
}

#[test]
fn decodes_the_feature_infos() {
    let mut payload = [0; 16];
    payload[..7].copy_from_slice(&[0x03, 0x00, 3, 4, 3, 1, 2]);
    let info = MultiPlatformInfo::from_payload(&payload);
    assert_eq!(
        info.capabilities,
        MultiPlatformCapabilities::OS_DETECTION | MultiPlatformCapabilities::SET_HOST_PLATFORM
    );
    assert_eq!(info.platform_count, 3);
    assert_eq!(info.descriptor_count, 4);
    assert_eq!(info.host_count, 3);
    assert_eq!(info.current_host, HostIndex::Slot(1));
    assert_eq!(info.current_host_platform, Some(2));

    payload[6] = 0xff;
    assert_eq!(
        MultiPlatformInfo::from_payload(&payload).current_host_platform,
        None
    );
}

#[test]
fn decodes_a_platform_change_report() {
    assert_eq!(
        PlatformChange::decode(&[1, 2, 2, 0, 0]),
        Some(PlatformChange {
            host_index: HostIndex::Slot(1),
            platform_index: Some(2),
            source: Some(PlatformSource::Manual),
        })
    );
    // An undefined platform and an unknown source still announce a change.
    assert_eq!(
        PlatformChange::decode(&[0, 0xff, 9]),
        Some(PlatformChange {
            host_index: HostIndex::Slot(0),
            platform_index: None,
            source: None,
        })
    );
}

#[test]
fn a_report_shorter_than_three_bytes_is_not_a_platform_change() {
    assert_eq!(PlatformChange::decode(&[1, 2]), None);
    assert_eq!(PlatformChange::decode(&[]), None);
}
