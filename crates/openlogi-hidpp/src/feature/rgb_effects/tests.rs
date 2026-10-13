//! Unit tests for `RgbEffects` payload parsing and event decoding.

use super::event::{RgbEffectsEvent, decode_event};
use super::types::{
    ActivityEventType, DisplayPersistencyCapabilities, LedBinIndex, PowerModeTarget,
    RgbClusterInfo, RgbDeviceInfo, RgbEffectInfo, RgbExtCapabilities, RgbNvCapabilities,
    RgbPersistence, RgbPowerMode, RgbPowerModeConfig, SlotInfoType,
};

#[test]
fn restoring_control_rejects_late_claim_and_different_notification_flags() {
    use std::sync::Arc;

    use super::{EventsNotificationFlags, RgbEffectsFeature, SwControlFlags};
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let (seen, incoming) = async_channel::unbounded();
        let _listener = channel.add_msg_listener_guarded(move |_, matched| {
            seen.try_send(matched).unwrap();
        });
        let feature = RgbEffectsFeature::new(channel, 3, 8);
        let mut restore = Box::pin(feature.set_sw_control(
            SwControlFlags::from_bits_retain(0x82),
            EventsNotificationFlags::from_bits_retain(0x45),
        ));
        assert!(futures::poll!(restore.as_mut()).is_pending());
        let header = MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: U4::from_lo(5),
            software_id: U4::from_lo(1),
        };
        for payload in [[1, 0x83, 0x45], [1, 0x82, 0x44], [0, 0x82, 0x45]] {
            handle
                .send_incoming(Message::Short(header, payload).into())
                .await;
            assert!(!incoming.recv().await.unwrap());
            assert!(futures::poll!(restore.as_mut()).is_pending());
        }
        handle
            .send_incoming(Message::Short(header, [1, 0x82, 0x45]).into())
            .await;
        restore.await.unwrap();
        assert!(incoming.recv().await.unwrap());
    });
}

fn nv_config_header() -> crate::protocol::v20::MessageHeader {
    crate::protocol::v20::MessageHeader {
        device_index: 3,
        feature_index: 8,
        function_id: crate::nibble::U4::from_lo(3),
        software_id: crate::nibble::U4::from_lo(1),
    }
}

#[test]
fn nv_config_write_hands_back_every_parameter_it_read() {
    use std::sync::Arc;

    use super::RgbEffectsFeature;
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::protocol::v20::Message;

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = RgbEffectsFeature::new(channel, 3, 8);

        let mut read = Box::pin(feature.get_nv_config(RgbNvCapabilities::BOOT_UP_EFFECT));
        assert!(futures::poll!(read.as_mut()).is_pending());
        let mut reply = [0u8; 16];
        reply[..10].copy_from_slice(&[0, 0x00, 0x01, 1, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        handle
            .send_incoming(Message::Long(nv_config_header(), reply).into())
            .await;
        let mut config = read.await.unwrap();
        assert_eq!(config.params, [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);

        config.state = 2;
        let mut write = Box::pin(feature.set_nv_config(&config));
        assert!(futures::poll!(write.as_mut()).is_pending());
        handle
            .send_incoming(Message::Long(nv_config_header(), reply).into())
            .await;
        write.await.unwrap();

        let written = handle.written_reports();
        let request = written.last().expect("setNvConfig was sent");
        assert_eq!(
            &request[4..14],
            &[1, 0x00, 0x01, 2, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            "only the state changes; parameters 3..=6 are not zeroed"
        );
    });
}

#[test]
fn nv_config_rejects_a_short_reply() {
    use std::sync::Arc;

    use super::RgbEffectsFeature;
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::protocol::v20::{Hidpp20Error, Message};

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = RgbEffectsFeature::new(channel, 3, 8);
        let mut read = Box::pin(feature.get_nv_config(RgbNvCapabilities::BOOT_UP_EFFECT));
        assert!(futures::poll!(read.as_mut()).is_pending());
        handle
            .send_incoming(Message::Short(nv_config_header(), [0, 0x00, 0x01]).into())
            .await;
        assert!(matches!(read.await, Err(Hidpp20Error::UnsupportedResponse)));
    });
}

#[test]
fn parses_device_info() {
    let mut payload = [0; 16];
    payload[0] = 0xff;
    payload[1] = 0xff;
    payload[2] = 2; // cluster count
    payload[3..5].copy_from_slice(&0x0001u16.to_be_bytes()); // bootUp
    payload[5..7].copy_from_slice(&0x0021u16.to_be_bytes()); // getZoneEffect + shutdown
    payload[7] = 3; // multicluster effects

    let info = RgbDeviceInfo::from_payload(&payload);
    assert_eq!(info.cluster_count, 2);
    assert!(
        info.nv_capabilities
            .contains(RgbNvCapabilities::BOOT_UP_EFFECT)
    );
    assert!(
        info.ext_capabilities
            .contains(RgbExtCapabilities::GET_ZONE_EFFECT)
    );
    assert!(info.ext_capabilities.contains(RgbExtCapabilities::SHUTDOWN));
    assert_eq!(info.multicluster_effect_count, 3);
}

#[test]
fn parses_cluster_info() {
    let mut payload = [0; 16];
    payload[0] = 1;
    payload[2..4].copy_from_slice(&2u16.to_be_bytes()); // location = Logo
    payload[4] = 5; // effects
    payload[5] = 0b101; // always_on + on_then_off
    payload[6] = 1; // effect persistency supported
    payload[7] = 0; // no multiled pattern

    let info = RgbClusterInfo::from_payload(&payload);
    assert_eq!(info.cluster_index, 1);
    assert_eq!(info.location, 2);
    assert_eq!(info.effects_number, 5);
    assert!(
        info.display_persistency
            .contains(DisplayPersistencyCapabilities::ALWAYS_ON)
    );
    assert!(
        info.display_persistency
            .contains(DisplayPersistencyCapabilities::ON_THEN_OFF)
    );
    assert!(info.effect_persistency);
    assert!(!info.multiled_pattern);
}

#[test]
fn parses_effect_info() {
    let mut payload = [0; 16];
    payload[0] = 0;
    payload[1] = 1;
    payload[2..4].copy_from_slice(&4u16.to_be_bytes()); // ColorWave
    payload[4..6].copy_from_slice(&0x0007u16.to_be_bytes());
    payload[6..8].copy_from_slice(&500u16.to_be_bytes());

    let info = RgbEffectInfo::from_payload(&payload);
    assert_eq!(info.cluster_effect_index, 1);
    assert_eq!(info.effect_id, 4);
    assert_eq!(info.effect_capabilities, 0x0007);
    assert_eq!(info.effect_period, 500);
}

#[test]
fn parses_power_mode_config() {
    let mut payload = [0; 16];
    payload[0] = 0; // getOrSet echo
    payload[1..3].copy_from_slice(&0x0003u16.to_be_bytes());
    payload[3..5].copy_from_slice(&60u16.to_be_bytes());
    payload[5..7].copy_from_slice(&300u16.to_be_bytes());

    let config = RgbPowerModeConfig::from_payload(&payload);
    assert_eq!(config.flags, 0x0003);
    assert_eq!(config.no_activity_timeout_to_power_save, 60);
    assert_eq!(config.no_activity_timeout_to_off, 300);
}

#[test]
fn decodes_effect_sync_event() {
    let mut payload = [0; 16];
    payload[0] = 0xff;
    payload[1..3].copy_from_slice(&2000u16.to_be_bytes());

    assert_eq!(
        decode_event(0, &payload),
        Some(RgbEffectsEvent::EffectSync {
            cluster_index: 0xff,
            effect_counter: 2000,
        })
    );
}

#[test]
fn decodes_user_activity_event() {
    let mut payload = [0; 16];
    payload[0] = 1;
    assert_eq!(
        decode_event(1, &payload),
        Some(RgbEffectsEvent::UserActivity(
            ActivityEventType::UserActivityDetected
        ))
    );
}

#[test]
fn decodes_cluster_changed_event() {
    let mut payload = [0; 16];
    payload[0] = 1;
    payload[1] = 2;
    payload[2..12].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    // Flags byte: persistence = VOLATILE (bit0), power-mode target = PowerSave (bit2).
    payload[12] = 0b101;

    assert_eq!(
        decode_event(2, &payload),
        Some(RgbEffectsEvent::ClusterChanged {
            cluster_index: 1,
            cluster_effect_index: 2,
            params: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            persistence: RgbPersistence::VOLATILE,
            power_mode: Some(PowerModeTarget::PowerSave),
        })
    );
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(7, &[0; 16]).is_none());
}

#[test]
fn maps_stable_enum_wire_values() {
    assert_eq!(RgbPowerMode::try_from(1u8).unwrap(), RgbPowerMode::FullRgb);
    assert_eq!(RgbPowerMode::try_from(3u8).unwrap(), RgbPowerMode::PowerOff);
    RgbPowerMode::try_from(0u8).unwrap_err();
    assert_eq!(u8::from(PowerModeTarget::PowerSave), 1);
    assert_eq!(
        LedBinIndex::try_from(2u8).unwrap(),
        LedBinIndex::CalibrationFactors
    );
    assert_eq!(
        SlotInfoType::try_from(6u8).unwrap(),
        SlotInfoType::EffectName21To31
    );
    SlotInfoType::try_from(7u8).unwrap_err();
}

/// Totality: the user-activity event must decode for any activity byte.
#[test]
fn keeps_user_activity_event_with_unknown_type() {
    for byte in 0..=u8::MAX {
        let mut payload = [0; 16];
        payload[0] = byte;
        assert_eq!(
            decode_event(1, &payload),
            Some(RgbEffectsEvent::UserActivity(ActivityEventType::from(byte))),
            "dropped user-activity event for byte {byte:#04x}"
        );
    }
}

/// Totality: the cluster-changed event must decode for any flags byte. The
/// 2-bit power-mode nibble can hold values 2 and 3 that the enum does not
/// model; those must surface as `None` without dropping the event.
#[test]
fn keeps_cluster_changed_event_with_unknown_power_mode() {
    for flags in 0..=u8::MAX {
        let mut payload = [0; 16];
        payload[0] = 1; // cluster_index sibling that must survive
        payload[12] = flags;
        let event = decode_event(2, &payload)
            .unwrap_or_else(|| panic!("dropped cluster-changed event for flags {flags:#04x}"));
        let RgbEffectsEvent::ClusterChanged { cluster_index, .. } = event else {
            panic!("expected ClusterChanged");
        };
        assert_eq!(cluster_index, 1);
    }
}

/// Answers one `getInfo` cluster-mode request for `cluster_index` with
/// `reply` and returns the wrapper's result.
fn cluster_info_reply(
    cluster_index: u8,
    reply: crate::protocol::v20::Message,
) -> Result<RgbClusterInfo, crate::protocol::v20::Hidpp20Error> {
    use std::sync::Arc;

    use super::RgbEffectsFeature;
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = RgbEffectsFeature::new(channel, 3, 8);
        handle.queue_response(reply.into());
        feature.get_cluster_info(cluster_index).await
    })
}

fn cluster_info_header() -> crate::protocol::v20::MessageHeader {
    crate::protocol::v20::MessageHeader {
        device_index: 3,
        feature_index: 8,
        function_id: crate::nibble::U4::from_lo(0),
        software_id: crate::nibble::U4::from_lo(1),
    }
}

/// A long cluster-info payload for `cluster_index` with five effects.
fn cluster_info_payload(cluster_index: u8) -> [u8; 16] {
    let mut payload = [0; 16];
    payload[0] = cluster_index;
    payload[2..4].copy_from_slice(&0x0001u16.to_be_bytes());
    payload[4] = 5;
    payload
}

#[test]
fn cluster_info_reads_the_requested_cluster() {
    use crate::protocol::v20::Message;

    let info = cluster_info_reply(
        1,
        Message::Long(cluster_info_header(), cluster_info_payload(1)),
    )
    .expect("matching long reply");
    assert_eq!(info.cluster_index, 1);
    assert_eq!(info.effects_number, 5);
}

#[test]
fn cluster_info_refuses_the_info_of_another_cluster() {
    // Options+ GetRgbClusterInfo compares byte 0 with the requested index and
    // fails with a protocol error otherwise.
    use crate::protocol::v20::{Hidpp20Error, Message};

    let reply = cluster_info_reply(
        1,
        Message::Long(cluster_info_header(), cluster_info_payload(0)),
    );
    assert!(matches!(reply, Err(Hidpp20Error::UnsupportedResponse)));
}

#[test]
fn cluster_info_refuses_a_short_reply() {
    // A short reply would zero-fill the effect count and persistency.
    use crate::protocol::v20::{Hidpp20Error, Message};

    let reply = cluster_info_reply(1, Message::Short(cluster_info_header(), [1, 0x00, 0x00]));
    assert!(matches!(reply, Err(Hidpp20Error::UnsupportedResponse)));
}
