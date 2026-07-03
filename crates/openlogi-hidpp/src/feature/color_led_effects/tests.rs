//! Unit tests for `ColorLedEffects` payload parsing and event decoding.

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

    assert!(matches!(
        ZoneInfo::from_payload(&payload),
        Err(Hidpp20Error::UnsupportedResponse)
    ));
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
    assert!(EffectId::try_from(12u16).is_err());
    assert_eq!(u16::from(EffectId::FixedColor), 1);
}

#[test]
fn validates_single_nv_capability() {
    assert!(validate_single_nv_capability(NvCapabilities::BOOT_UP_EFFECT).is_ok());
    assert!(matches!(
        validate_single_nv_capability(NvCapabilities::empty()),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    ));
    assert!(matches!(
        validate_single_nv_capability(NvCapabilities::BOOT_UP_EFFECT | NvCapabilities::DEMO),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    ));
}
