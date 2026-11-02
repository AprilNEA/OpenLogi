//! Force-button wire exchanges and capability-bound write regressions.

use std::sync::Arc;

use super::{
    ForceButtonCapabilities, ForceButtonError, ForceButtonIndex, ForceSensingButtonFeature,
    ForceThreshold,
};
use crate::{
    channel::mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
    feature::{CreatableFeature, registry},
    nibble::U4,
    protocol::v20::{Hidpp20Error, Message, MessageHeader},
};

async fn device() -> (ForceSensingButtonFeature, MockRawHidHandle) {
    let (raw, handle) = MockRawHidChannel::new();
    let channel = Arc::new(channel_with_reader(raw).await);
    (ForceSensingButtonFeature::new(channel, 3, 6), handle)
}

fn header(function: u8) -> MessageHeader {
    MessageHeader {
        device_index: 3,
        feature_index: 6,
        function_id: U4::from_lo(function),
        software_id: U4::from_lo(1),
    }
}

fn info(flags: [u8; 2]) -> [u8; 16] {
    // Synthetic data: default 5089, maximum 6768, minimum 4071.
    // Literal byte pairs distinguish wrong field order or endianness.
    let mut payload = [0; 16];
    payload[..8].copy_from_slice(&[flags[0], flags[1], 0x13, 0xe1, 0x1a, 0x70, 0x0f, 0xe7]);
    payload
}

fn queue_info(handle: &MockRawHidHandle, payload: [u8; 16]) {
    handle.queue_response(Message::Long(header(1), payload).into());
}

#[test]
fn registry_provides_the_version_zero_implementation() {
    let versions = registry::lookup_version(0x19c0, 0).expect("known force feature");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].starting_version, 0);
    futures::executor::block_on(async {
        let (raw, _) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let (id, _) = (versions[0].producer)(channel, 3, 6);
        assert_eq!(id, std::any::TypeId::of::<ForceSensingButtonFeature>());
    });
}

#[test]
fn count_reads_function_zero_without_writing_a_threshold() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        handle.queue_response(Message::Short(header(0), [2, 0, 0]).into());
        assert_eq!(feature.get_button_count().await.unwrap(), 2);
        assert_eq!(handle.written_reports(), [vec![0x10, 3, 6, 0x01, 0, 0, 0]]);
    });
}

#[test]
fn metadata_reads_the_requested_button_and_preserves_unknown_flags() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        queue_info(&handle, info([0x80, 0x01]));
        let button = feature
            .get_button_info(ForceButtonIndex::from(1))
            .await
            .unwrap();
        assert_eq!(u8::from(button.index()), 1);
        assert_eq!(button.capabilities().bits(), 0x8001);
        assert!(
            button
                .capabilities()
                .contains(ForceButtonCapabilities::CHANGEABLE)
        );
        assert_eq!(u16::from(button.default_threshold()), 5089);
        assert_eq!(
            button.range(),
            ForceThreshold::from(4071)..=ForceThreshold::from(6768)
        );
        assert_eq!(handle.written_reports(), [vec![0x10, 3, 6, 0x11, 1, 0, 0]]);
    });
}

#[test]
fn metadata_refuses_a_short_response() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        handle.queue_response(Message::Short(header(1), [0, 1, 0x13]).into());
        assert!(matches!(
            feature.get_button_info(ForceButtonIndex::from(0)).await,
            Err(Hidpp20Error::UnsupportedResponse)
        ));
        assert_eq!(handle.written_reports().len(), 1);
    });
}

#[test]
fn metadata_refuses_reversed_ranges_and_out_of_range_defaults() {
    futures::executor::block_on(async {
        for bytes in [
            [0x13, 0xe1, 0x0f, 0xe7, 0x1a, 0x70], // maximum below minimum
            [0x0f, 0xe6, 0x1a, 0x70, 0x0f, 0xe7], // default below minimum
            [0x1a, 0x71, 0x1a, 0x70, 0x0f, 0xe7], // default above maximum
        ] {
            let (feature, handle) = device().await;
            let mut payload = info([0, 1]);
            payload[2..8].copy_from_slice(&bytes);
            queue_info(&handle, payload);
            assert!(matches!(
                feature.get_button_info(ForceButtonIndex::from(0)).await,
                Err(Hidpp20Error::UnsupportedResponse)
            ));
        }
    });
}

#[test]
fn config_reads_current_threshold_not_default_or_padding() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        queue_info(&handle, info([0, 1]));
        let button = feature
            .get_button_info(ForceButtonIndex::from(1))
            .await
            .unwrap();
        handle.queue_response(Message::Short(header(2), [0x12, 0x34, 0xa5]).into());
        assert_eq!(u16::from(button.get_button_config().await.unwrap()), 0x1234);
        assert_eq!(handle.written_reports()[1], [0x10, 3, 6, 0x21, 1, 0, 0]);
    });
}

#[test]
fn writes_inclusive_bounds_as_index_and_big_endian_threshold() {
    futures::executor::block_on(async {
        for (value, bytes) in [(4071, [0x0f, 0xe7]), (6768, [0x1a, 0x70])] {
            let (feature, handle) = device().await;
            queue_info(&handle, info([0, 1]));
            let button = feature
                .get_button_info(ForceButtonIndex::from(1))
                .await
                .unwrap();
            handle.queue_response(Message::Short(header(3), [0xa5, 0, 0]).into());
            button
                .set_button_config(ForceThreshold::from(value))
                .await
                .unwrap();
            assert_eq!(
                handle.written_reports(),
                [
                    vec![0x10, 3, 6, 0x11, 1, 0, 0],
                    vec![0x10, 3, 6, 0x31, 1, bytes[0], bytes[1]]
                ]
            );
            // Acknowledgement bytes do not pretend to be read-back state.
            handle.queue_response(Message::Short(header(2), [0x13, 0xe2, 0]).into());
            assert_eq!(u16::from(button.get_button_config().await.unwrap()), 5090);
        }
    });
}

#[test]
fn refuses_out_of_range_thresholds_without_any_write() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        queue_info(&handle, info([0, 1]));
        let button = feature
            .get_button_info(ForceButtonIndex::from(0))
            .await
            .unwrap();
        for value in [0, 4070, 6769, u16::MAX] {
            assert!(matches!(
                button.set_button_config(ForceThreshold::from(value)).await,
                Err(ForceButtonError::OutOfRange)
            ));
        }
        assert_eq!(handle.written_reports(), [vec![0x10, 3, 6, 0x11, 0, 0, 0]]);
    });
}

#[test]
fn unnamed_flags_do_not_grant_write_capability() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        queue_info(&handle, info([0x80, 0]));
        let button = feature
            .get_button_info(ForceButtonIndex::from(0))
            .await
            .unwrap();
        assert!(matches!(
            button.set_button_config(ForceThreshold::from(5089)).await,
            Err(ForceButtonError::ReadOnly)
        ));
        assert_eq!(handle.written_reports().len(), 1);
    });
}

#[test]
fn write_failure_preserves_the_protocol_error() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        queue_info(&handle, info([0, 1]));
        let button = feature
            .get_button_info(ForceButtonIndex::from(0))
            .await
            .unwrap();
        handle.fail_writes();
        assert!(matches!(
            button.set_button_config(ForceThreshold::from(5089)).await,
            Err(ForceButtonError::Protocol(Hidpp20Error::Channel(_)))
        ));
    });
}

#[test]
fn device_rejection_preserves_the_exact_feature_error() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        queue_info(&handle, info([0, 1]));
        let button = feature
            .get_button_info(ForceButtonIndex::from(0))
            .await
            .unwrap();
        // Device 3 rejects feature 6, function/software byte 0x31, with
        // INVALID_ARGUMENT (0x02). This is a device response, not failed I/O.
        handle.queue_response(crate::channel::HidppMessage::Short([
            3, 0xff, 6, 0x31, 0x02, 0,
        ]));
        assert!(matches!(
            button.set_button_config(ForceThreshold::from(5089)).await,
            Err(ForceButtonError::Protocol(Hidpp20Error::Feature(
                crate::protocol::v20::ErrorType::InvalidArgument
            )))
        ));
        assert_eq!(
            handle.written_reports()[1],
            [0x10, 3, 6, 0x31, 0, 0x13, 0xe1]
        );
    });
}
