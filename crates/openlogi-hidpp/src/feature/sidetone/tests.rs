//! Sidetone wire exchanges, from the G535's replies.

use std::sync::Arc;

use super::SidetoneFeature;
use crate::{
    channel::mock::{MockRawHidChannel, MockRawHidHandle, channel_with_reader},
    feature::{CreatableFeature, registry},
    nibble::U4,
    protocol::v20::{Hidpp20Error, Message, MessageHeader},
};

async fn device() -> (SidetoneFeature, MockRawHidHandle) {
    let (raw, handle) = MockRawHidChannel::new();
    let channel = Arc::new(channel_with_reader(raw).await);
    (SidetoneFeature::new(channel, 0xff, 4), handle)
}

fn reply(function: u8, level: u8) -> Message {
    let header = MessageHeader {
        device_index: 0xff,
        feature_index: 4,
        function_id: U4::from_lo(function),
        software_id: U4::from_lo(1),
    };
    Message::Short(header, [level, 0, 0])
}

#[test]
fn registry_accepts_the_version_zero_feature() {
    let versions = registry::lookup_version(0x8300, 0).expect("known sidetone feature");
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].starting_version, 0);
}

#[test]
fn reads_the_level_with_function_zero() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        handle.queue_response(reply(0, 40).into());
        assert_eq!(feature.get_sidetone_level().await.unwrap(), 40);
        assert_eq!(
            handle.written_reports(),
            [vec![0x10, 0xff, 4, 0x01, 0, 0, 0]]
        );
    });
}

#[test]
fn a_write_returns_the_level_the_device_echoes() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        handle.queue_response(reply(1, 30).into());
        assert_eq!(feature.set_sidetone_level(30).await.unwrap(), 30);
        assert_eq!(
            handle.written_reports(),
            [vec![0x10, 0xff, 4, 0x11, 30, 0, 0]]
        );
    });
}

#[test]
fn a_level_above_one_hundred_is_an_unsupported_response() {
    futures::executor::block_on(async {
        let (feature, handle) = device().await;
        handle.queue_response(reply(0, 101).into());
        assert!(matches!(
            feature.get_sidetone_level().await,
            Err(Hidpp20Error::UnsupportedResponse)
        ));
    });
}
