//! Unit tests for `SolarKeyboardDashboard` event decoding, using the spec's
//! worked examples (battery 96%, light 0 / 319 lux).

use super::LedId;
use super::event::{SolarEvent, SolarStatus, decode_event};

#[test]
fn decodes_battery_event() {
    // Spec example: 11 03 06 00 60 00 00 ... → battery 96%, light 0.
    let mut payload = [0; 16];
    payload[0] = 0x60;

    assert_eq!(
        decode_event(0, &payload),
        Some(SolarEvent::Battery(SolarStatus {
            battery_level: 96,
            light_level: 0,
        }))
    );
}

#[test]
fn decodes_light_measure_event() {
    // Spec example: 11 03 06 10 60 01 3F ... → battery 96%, light 0x013F = 319.
    let mut payload = [0; 16];
    payload[0] = 0x60;
    payload[1] = 0x01;
    payload[2] = 0x3f;

    assert_eq!(
        decode_event(1, &payload),
        Some(SolarEvent::LightMeasure(SolarStatus {
            battery_level: 96,
            light_level: 319,
        }))
    );
}

#[test]
fn decodes_check_light_button_event() {
    // Spec example: 11 03 06 20 60 00 00 ... → battery 96%, light 0.
    let mut payload = [0; 16];
    payload[0] = 0x60;

    assert_eq!(
        decode_event(2, &payload),
        Some(SolarEvent::CheckLightButton(SolarStatus {
            battery_level: 96,
            light_level: 0,
        }))
    );
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(3, &[0; 16]).is_none());
}

#[test]
fn maps_led_wire_values() {
    assert_eq!(u8::from(LedId::Off), 0);
    assert_eq!(LedId::try_from(3u8).unwrap(), LedId::Green);
    assert!(LedId::try_from(4u8).is_err());
}
