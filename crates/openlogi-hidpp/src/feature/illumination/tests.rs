//! Unit tests for `Illumination` payload parsing and event decoding.

use super::event::{IlluminationEvent, decode_event};
use super::types::{
    BrightnessClampedSource, ControlCapabilities, ControlInfo, IlluminationState, LevelConfig,
    SetLevels,
};
use crate::protocol::v20::{ErrorType, Hidpp20Error};

#[test]
fn parses_control_info() {
    let mut payload = [0; 16];
    payload[0] = 0b0000_1011; // events + linear + dynamic max
    payload[1..3].copy_from_slice(&100u16.to_be_bytes());
    payload[3..5].copy_from_slice(&1000u16.to_be_bytes());
    payload[5..7].copy_from_slice(&10u16.to_be_bytes());
    payload[7] = 0x05;

    let info = ControlInfo::from_payload(&payload);
    assert!(info.capabilities.contains(ControlCapabilities::HAS_EVENTS));
    assert!(
        info.capabilities
            .contains(ControlCapabilities::HAS_LINEAR_LEVELS)
    );
    assert!(
        info.capabilities
            .contains(ControlCapabilities::HAS_DYNAMIC_MAXIMUM)
    );
    assert!(
        !info
            .capabilities
            .contains(ControlCapabilities::HAS_NON_LINEAR_LEVELS)
    );
    assert_eq!(info.min, 100);
    assert_eq!(info.max, 1000);
    assert_eq!(info.resolution, 10);
    assert_eq!(info.max_levels, 5);
}

#[test]
fn parses_linear_levels() {
    let mut payload = [0; 16];
    payload[0] = 1; // linear
    payload[2..4].copy_from_slice(&100u16.to_be_bytes());
    payload[4..6].copy_from_slice(&500u16.to_be_bytes());
    payload[6..8].copy_from_slice(&50u16.to_be_bytes());

    assert_eq!(
        LevelConfig::from_payload(&payload),
        LevelConfig::Linear {
            min: 100,
            max: 500,
            step: 50
        }
    );
}

#[test]
fn parses_non_linear_levels() {
    let mut payload = [0; 16];
    // validCount = 3 (bits 5..7), linear bit clear.
    payload[0] = 3 << 5;
    // startIndex = 1 (high nibble), levelCount = 6 (low nibble).
    payload[1] = (1 << 4) | 6;
    payload[2..4].copy_from_slice(&200u16.to_be_bytes());
    payload[4..6].copy_from_slice(&400u16.to_be_bytes());
    payload[6..8].copy_from_slice(&800u16.to_be_bytes());

    assert_eq!(
        LevelConfig::from_payload(&payload),
        LevelConfig::NonLinear {
            start_index: 1,
            level_count: 6,
            values: vec![200, 400, 800],
        }
    );
}

#[test]
fn encodes_reset_levels() {
    let payload = SetLevels::Reset.to_payload().unwrap();
    assert_eq!(payload[0], 1 << 1);
    assert!(payload[1..].iter().all(|&b| b == 0));
}

#[test]
fn encodes_linear_levels() {
    let payload = SetLevels::Linear {
        min: 100,
        max: 500,
        step: 50,
    }
    .to_payload()
    .unwrap();
    assert_eq!(payload[0], 1);
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 100);
    assert_eq!(u16::from_be_bytes([payload[4], payload[5]]), 500);
    assert_eq!(u16::from_be_bytes([payload[6], payload[7]]), 50);
}

#[test]
fn encodes_non_linear_levels() {
    let payload = SetLevels::NonLinear {
        start_index: 2,
        level_count: 5,
        values: vec![100, 200],
    }
    .to_payload()
    .unwrap();

    assert_eq!(payload[0], 2 << 5); // validCount = 2, linear/reset clear
    assert_eq!(payload[1], (2 << 4) | 5); // startIndex 2, levelCount 5
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 100);
    assert_eq!(u16::from_be_bytes([payload[4], payload[5]]), 200);
}

#[test]
fn non_linear_round_trips_through_decoder() {
    // A non-linear set payload, read back through the get decoder, must agree on
    // the value list (the get response uses the same field layout).
    let payload = SetLevels::NonLinear {
        start_index: 0,
        level_count: 3,
        values: vec![50, 150, 300],
    }
    .to_payload()
    .unwrap();

    assert_eq!(
        LevelConfig::from_payload(&payload),
        LevelConfig::NonLinear {
            start_index: 0,
            level_count: 3,
            values: vec![50, 150, 300],
        }
    );
}

#[test]
fn rejects_invalid_non_linear_levels() {
    for levels in [
        SetLevels::NonLinear {
            start_index: 0,
            level_count: 1,
            values: vec![],
        },
        SetLevels::NonLinear {
            start_index: 0,
            level_count: 8,
            values: vec![1, 2, 3, 4, 5, 6, 7, 8],
        },
        SetLevels::NonLinear {
            start_index: 0x10,
            level_count: 1,
            values: vec![1],
        },
        SetLevels::NonLinear {
            start_index: 0,
            level_count: 0x10,
            values: vec![1],
        },
    ] {
        assert!(matches!(
            levels.to_payload(),
            Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
        ));
    }
}

#[test]
fn decodes_state_and_value_events() {
    let mut on = [0; 16];
    on[0] = 1;
    assert_eq!(
        decode_event(0, &on),
        Some(IlluminationEvent::IlluminationChanged(
            IlluminationState::On
        ))
    );

    let mut brightness = [0; 16];
    brightness[0..2].copy_from_slice(&750u16.to_be_bytes());
    assert_eq!(
        decode_event(1, &brightness),
        Some(IlluminationEvent::BrightnessChanged(750))
    );

    let mut temp = [0; 16];
    temp[0..2].copy_from_slice(&5000u16.to_be_bytes());
    assert_eq!(
        decode_event(2, &temp),
        Some(IlluminationEvent::ColorTemperatureChanged(5000))
    );

    let mut eff = [0; 16];
    eff[0..2].copy_from_slice(&600u16.to_be_bytes());
    assert_eq!(
        decode_event(3, &eff),
        Some(IlluminationEvent::BrightnessEffectiveMaxChanged(600))
    );
}

#[test]
fn decodes_brightness_clamped_event() {
    let mut payload = [0; 16];
    payload[0] = 2; // Button
    payload[1..3].copy_from_slice(&600u16.to_be_bytes());

    assert_eq!(
        decode_event(4, &payload),
        Some(IlluminationEvent::BrightnessClamped {
            source: BrightnessClampedSource::Button,
            brightness: 600,
        })
    );
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(9, &[0; 16]).is_none());
}
