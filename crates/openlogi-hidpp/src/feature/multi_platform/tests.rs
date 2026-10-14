//! Unit tests for `MultiPlatform` descriptor parsing.

use super::{MultiPlatformFeature, OsMask, PlatformDescriptor};

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
fn reads_multiplatform_capabilities_and_descriptor_count() {
    use std::sync::Arc;

    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = MultiPlatformFeature::new(channel, 3, 8);
        let header = MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: U4::from_lo(0),
            software_id: U4::from_lo(1),
        };
        let mut read = Box::pin(feature.get_feature_infos());
        assert!(futures::poll!(read.as_mut()).is_pending());

        let [request] = &handle.written_reports()[..] else {
            panic!("one request");
        };
        assert_eq!(request[3] >> 4, 0);

        let mut payload = [0; 16];
        payload[..7].copy_from_slice(&[0x02, 0x00, 0x04, 0x05, 0x03, 0x01, 0x00]);
        handle
            .send_incoming(Message::Long(header, payload).into())
            .await;
        let info = read.await.expect("feature info response");
        assert!(
            info.capabilities
                .contains(super::MultiPlatformCapabilities::SET_HOST_PLATFORM)
        );
        assert_eq!(info.descriptor_count, 4);
    });
}

#[test]
fn sets_platform_for_the_selected_host() {
    use std::sync::Arc;

    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::feature::hosts_info::HostIndex;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = MultiPlatformFeature::new(channel, 3, 8);
        let header = MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: U4::from_lo(3),
            software_id: U4::from_lo(1),
        };
        let mut write = Box::pin(feature.set_host_platform(HostIndex::Current, 2));
        assert!(futures::poll!(write.as_mut()).is_pending());

        let [request] = &handle.written_reports()[..] else {
            panic!("one request");
        };
        assert_eq!(request[3] >> 4, 3);
        assert_eq!(request[4..7], [0xff, 2, 0]);

        handle
            .send_incoming(Message::Long(header, [0; 16]).into())
            .await;
        write
            .await
            .expect("platform write should receive its response");
    });
}
