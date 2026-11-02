//! Unit tests for `ColorLedEffects` payload parsing and event decoding.

use std::assert_matches;

use super::event::{ColorLedEffectsEvent, decode_event};
use super::types::{
    ColorLedInfo, EffectId, EffectSettings, ExtCapabilities, LedBinIndex, LedBinInfo,
    LocationEffect, NvCapabilities, PersistencyCapabilities, ZoneEffect, ZoneEffectInfo, ZoneInfo,
};
use super::validate_single_nv_capability;
use crate::protocol::v20::{ErrorType, Hidpp20Error};

#[test]
fn parses_info() {
    let mut payload = [0; 16];
    payload[0] = 3;
    payload[1..3].copy_from_slice(&0x0005u16.to_be_bytes()); // bootUp + userDemo
    payload[3..5].copy_from_slice(&0x0001u16.to_be_bytes()); // getZoneEffect supported

    let info = ColorLedInfo::from_payload(&payload);
    assert_eq!(info.zone_count, 3);
    assert!(
        info.nv_capabilities
            .contains(NvCapabilities::BOOT_UP_EFFECT)
    );
    assert!(
        info.nv_capabilities
            .contains(NvCapabilities::USER_DEMO_MODE)
    );
    assert!(!info.nv_capabilities.contains(NvCapabilities::DEMO));
    assert!(
        info.ext_capabilities
            .contains(ExtCapabilities::GET_ZONE_EFFECT)
    );
}

#[test]
fn parses_zone_info() {
    let mut payload = [0; 16];
    payload[0] = 1;
    payload[1..3].copy_from_slice(&2u16.to_be_bytes()); // Logo
    payload[3] = 4;
    payload[4] = 0b101; // always_on + on_then_off

    let zone = ZoneInfo::from_payload(&payload).unwrap();
    assert_eq!(zone.zone_index, 1);
    assert_eq!(zone.location, LocationEffect::Logo);
    assert_eq!(zone.effects_number, 4);
    assert!(
        zone.persistency
            .contains(PersistencyCapabilities::ALWAYS_ON)
    );
    assert!(
        zone.persistency
            .contains(PersistencyCapabilities::ON_THEN_OFF)
    );
    assert!(
        !zone
            .persistency
            .contains(PersistencyCapabilities::ALWAYS_OFF)
    );
}

#[test]
fn rejects_unknown_zone_location() {
    let mut payload = [0; 16];
    payload[1..3].copy_from_slice(&99u16.to_be_bytes());

    assert_matches!(
        ZoneInfo::from_payload(&payload),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn parses_zone_effect_info() {
    let mut payload = [0; 16];
    payload[0] = 0;
    payload[1] = 2;
    payload[2..4].copy_from_slice(&4u16.to_be_bytes()); // ColorWave
    payload[4..6].copy_from_slice(&0x0003u16.to_be_bytes());
    payload[6..8].copy_from_slice(&1000u16.to_be_bytes());

    let info = ZoneEffectInfo::from_payload(&payload).unwrap();
    assert_eq!(info.zone_effect_index, 2);
    assert_eq!(info.effect_id, EffectId::ColorWave);
    assert_eq!(info.effect_capabilities, 0x0003);
    assert_eq!(info.effect_period, 1000);
}

#[test]
fn parses_effect_settings() {
    let mut payload = [0; 16];
    payload[0] = 1;
    payload[1..4].copy_from_slice(&[0x11, 0x22, 0x33]);
    payload[4..6].copy_from_slice(&2000u16.to_be_bytes());
    payload[6] = 80;
    payload[7] = 1;

    let settings = EffectSettings::from_payload(&payload);
    assert_eq!(settings.zone_index, 1);
    assert_eq!(settings.color.red, 0x11);
    assert_eq!(settings.color.green, 0x22);
    assert_eq!(settings.color.blue, 0x33);
    assert_eq!(settings.period, 2000);
    assert_eq!(settings.brightness, 80);
    assert_eq!(settings.param, 1);
}

#[test]
fn parses_zone_effect_params() {
    let mut payload = [0; 16];
    payload[0] = 2;
    payload[1] = 1;
    payload[2..12].copy_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);

    let effect = ZoneEffect::from_payload(&payload);
    assert_eq!(effect.zone_index, 2);
    assert_eq!(effect.zone_effect_index, 1);
    assert_eq!(effect.params, [10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
}

#[test]
fn parses_led_bin_info() {
    let mut payload = [0; 16];
    payload[0] = 0;
    payload[1] = 2; // CalibrationFactors
    payload[2..4].copy_from_slice(&100u16.to_be_bytes());
    payload[4..6].copy_from_slice(&200u16.to_be_bytes());
    payload[6..8].copy_from_slice(&300u16.to_be_bytes());
    payload[8..10].copy_from_slice(&400u16.to_be_bytes());

    let bin = LedBinInfo::from_payload(&payload).unwrap();
    assert_eq!(bin.led_bin_index, LedBinIndex::CalibrationFactors);
    assert_eq!(bin.red, 100);
    assert_eq!(bin.green, 200);
    assert_eq!(bin.blue, 300);
    assert_eq!(bin.white, 400);
}

#[test]
fn decodes_sync_effect_event() {
    let mut payload = [0; 16];
    payload[0] = 0xff; // all zones
    payload[1..3].copy_from_slice(&1234u16.to_be_bytes());

    assert_eq!(
        decode_event(0, &payload),
        Some(ColorLedEffectsEvent::SyncEffect {
            zone_index: 0xff,
            effect_counter: 1234,
        })
    );
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(5, &[0; 16]).is_none());
}

#[test]
fn maps_effect_id_wire_values() {
    assert_eq!(EffectId::try_from(0u16).unwrap(), EffectId::Disabled);
    assert_eq!(EffectId::try_from(1u16).unwrap(), EffectId::FixedColor);
    assert_eq!(EffectId::try_from(11u16).unwrap(), EffectId::Ripple);
    EffectId::try_from(12u16).unwrap_err();
    assert_eq!(u16::from(EffectId::FixedColor), 1);
}

#[test]
fn validates_single_nv_capability() {
    validate_single_nv_capability(NvCapabilities::BOOT_UP_EFFECT).unwrap();
    assert_matches!(
        validate_single_nv_capability(NvCapabilities::empty()),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    );
    assert_matches!(
        validate_single_nv_capability(NvCapabilities::BOOT_UP_EFFECT | NvCapabilities::DEMO),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    );
}

/// Answers one `getZoneInfo(zone_index)` request with `reply` and returns the
/// wrapper's result.
fn zone_info_reply(
    zone_index: u8,
    reply: crate::protocol::v20::Message,
) -> Result<ZoneInfo, Hidpp20Error> {
    use std::sync::Arc;

    use super::ColorLedEffectsFeature;
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = ColorLedEffectsFeature::new(channel, 3, 8);
        handle.queue_response(reply.into());
        feature.get_zone_info(zone_index).await
    })
}

fn zone_info_header() -> crate::protocol::v20::MessageHeader {
    crate::protocol::v20::MessageHeader {
        device_index: 3,
        feature_index: 8,
        function_id: crate::nibble::U4::from_lo(1),
        software_id: crate::nibble::U4::from_lo(1),
    }
}

/// A long `getZoneInfo` payload for `zone_index`: primary location, three
/// effects, volatile persistency.
fn zone_info_payload(zone_index: u8) -> [u8; 16] {
    let mut payload = [0; 16];
    payload[0] = zone_index;
    payload[1..3].copy_from_slice(&1u16.to_be_bytes());
    payload[3] = 3;
    payload[4] = 0x01;
    payload
}

#[test]
fn zone_info_reads_the_requested_zone() {
    use crate::protocol::v20::Message;

    let info = zone_info_reply(2, Message::Long(zone_info_header(), zone_info_payload(2)))
        .expect("matching long reply");
    assert_eq!(info.zone_index, 2);
    assert_eq!(info.location, LocationEffect::Primary);
    assert_eq!(info.effects_number, 3);
}

#[test]
fn zone_info_refuses_the_info_of_another_zone() {
    // Options+ GetZoneInfo compares byte 0 with the requested index and fails
    // with a protocol error otherwise.
    use crate::protocol::v20::Message;

    let reply = zone_info_reply(2, Message::Long(zone_info_header(), zone_info_payload(1)));
    assert_matches!(reply, Err(Hidpp20Error::UnsupportedResponse));
}

#[test]
fn zone_info_refuses_a_short_reply() {
    // A short reply would zero-fill the effect count and persistency.
    use crate::protocol::v20::Message;

    let reply = zone_info_reply(2, Message::Short(zone_info_header(), [2, 0x00, 0x01]));
    assert_matches!(reply, Err(Hidpp20Error::UnsupportedResponse));
}

/// Walks the feature table of a mock device that lists `0x8070` at
/// `version`, and returns whether the device then provides
/// `(ColorLedEffectsFeature, ColorLedEffectsV1Feature)`.
fn enumerated_color_led_effects(version: u8) -> (bool, bool) {
    use std::sync::Arc;

    use super::{ColorLedEffectsFeature, ColorLedEffectsV1Feature};
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::device::Device;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    let header = |feature_index, function| MessageHeader {
        device_index: 3,
        feature_index,
        function_id: U4::from_lo(function),
        software_id: U4::from_lo(1),
    };
    // A `FeatureSet.getFeatureId` reply: the id, no type flags, the version.
    let entry = |id: u16, version: u8| {
        let mut payload = [0; 16];
        payload[..2].copy_from_slice(&id.to_be_bytes());
        payload[3] = version;
        payload
    };
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        for reply in [
            // Root ping: HID++ 4.2.
            Message::Short(header(0, 1), [4, 2, 0]),
            // Root getFeature(0x0001): FeatureSet sits at index 1.
            Message::Short(header(0, 0), [1, 0, 0]),
            // FeatureSet: two entries after the root.
            Message::Short(header(1, 0), [2, 0, 0]),
            Message::Long(header(1, 1), entry(0x0001, 0)),
            Message::Long(header(1, 1), entry(0x8070, version)),
        ] {
            handle.queue_response(reply.into());
        }
        let channel = Arc::new(channel_with_reader(raw).await);
        let mut device = Device::new(channel, 3)
            .await
            .expect("the mock device answers the ping");
        device
            .enumerate_features()
            .await
            .expect("the mock device answers every table entry");
        (
            device.provides_feature::<ColorLedEffectsFeature>(),
            device.provides_feature::<ColorLedEffectsV1Feature>(),
        )
    })
}

#[test]
fn a_version_0_device_gets_no_version_1_functions() {
    // Logi Options+ 2.8 sends functions 4, 5, 7, 8 and 13 only to version 1
    // or later (static analysis). The zone and effect functions stay.
    assert_eq!(enumerated_color_led_effects(0), (true, false));
}

#[test]
fn a_version_1_or_later_device_gets_both_function_sets() {
    assert_eq!(enumerated_color_led_effects(1), (true, true));
    assert_eq!(enumerated_color_led_effects(7), (true, true));
}

#[test]
fn version_1_functions_keep_their_wire_layout() {
    use std::sync::Arc;

    use super::ColorLedEffectsV1Feature;
    use super::types::{SwControl, SwControlState};
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::protocol::v20::{Message, MessageHeader};

    let header = |function| MessageHeader {
        device_index: 3,
        feature_index: 8,
        function_id: crate::nibble::U4::from_lo(function),
        software_id: crate::nibble::U4::from_lo(1),
    };
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = ColorLedEffectsV1Feature::new(channel, 3, 8);

        // getSwControl (function 7): byte 0 is the owner, byte 1 the
        // sync-event flag.
        handle.queue_response(Message::Short(header(7), [0x01, 0x01, 0x00]).into());
        let state = feature.get_sw_control().await.expect("a valid reply");
        assert_eq!(
            state,
            SwControlState {
                control: SwControl::Software,
                sync_events: true,
            }
        );

        // synchronizeEffect (function 13): zone, reserved, big-endian drift.
        handle.queue_response(Message::Long(header(13), [0; 16]).into());
        feature
            .synchronize_effect(0xff, -2)
            .await
            .expect("an acknowledged write");

        let written = handle.written_reports();
        assert_eq!(written[0], [0x10, 3, 8, 0x71, 0, 0, 0]);
        assert_eq!(written[1][..8], [0x11, 3, 8, 0xd1, 0xff, 0x00, 0xff, 0xfe]);
    });
}
