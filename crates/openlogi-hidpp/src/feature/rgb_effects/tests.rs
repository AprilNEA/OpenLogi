//! Unit tests for `RgbEffects` payload parsing and event decoding.

use super::event::{RgbEffectsEvent, decode_event};
use super::types::{
    ActivityEventType, DisplayPersistencyCapabilities, LedBinIndex, PowerModeTarget,
    RgbClusterInfo, RgbDeviceInfo, RgbEffectInfo, RgbExtCapabilities, RgbNvCapabilities,
    RgbPersistence, RgbPowerMode, RgbPowerModeConfig, SlotInfoType,
};

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
            power_mode: PowerModeTarget::PowerSave,
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
    assert!(RgbPowerMode::try_from(0u8).is_err());
    assert_eq!(u8::from(PowerModeTarget::PowerSave), 1);
    assert_eq!(
        LedBinIndex::try_from(2u8).unwrap(),
        LedBinIndex::CalibrationFactors
    );
    assert_eq!(
        SlotInfoType::try_from(6u8).unwrap(),
        SlotInfoType::EffectName21To31
    );
    assert!(SlotInfoType::try_from(7u8).is_err());
}
