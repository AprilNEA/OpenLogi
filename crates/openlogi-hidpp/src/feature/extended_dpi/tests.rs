//! Unit tests for `ExtendedAdjustableDpi` payload parsing and event decoding.

use std::assert_matches;

use super::event::{ExtendedDpiEvent, decode_event};
use super::types::{
    DpiCalibrationCorrection, DpiDirection, DpiRange, Lod, parse_dpi_list, parse_dpi_ranges,
    parse_lod_list, terminated_word_len,
};
use crate::protocol::v20::{ErrorType, Hidpp20Error};

#[test]
fn parses_fixed_dpi_ranges_pws_example() {
    // Spec example: a PWS mouse supporting only 400, 800 and 1200 DPI.
    let stream = [0x01, 0x90, 0x03, 0x20, 0x04, 0xb0, 0x00, 0x00];

    assert_eq!(
        parse_dpi_ranges(&stream).unwrap(),
        [
            DpiRange::Fixed(400),
            DpiRange::Fixed(800),
            DpiRange::Fixed(1200),
        ]
    );
}

#[test]
fn parses_stepped_dpi_ranges_gaming_example() {
    // Spec example: 100..1000 step 1, then 1000..32000 step 100.
    let stream = [
        0x00, 0x64, 0xe0, 0x01, 0x03, 0xe8, 0xe0, 0x64, 0x7d, 0x00, 0x00, 0x00,
    ];

    assert_eq!(
        parse_dpi_ranges(&stream).unwrap(),
        [
            DpiRange::Stepped {
                from: 100,
                to: 1000,
                step: 1
            },
            DpiRange::Stepped {
                from: 1000,
                to: 32000,
                step: 100
            },
        ]
    );
}

#[test]
fn parses_dpi_ranges_split_across_pages() {
    // Spec example whose seventh word (0x03e8) straddles the page boundary: the
    // MSB ends page 0 and the LSB starts page 1. The accumulated stream parses as
    // five chained stepped ranges.
    let page0 = [
        0x00, 0x64, 0xe0, 0x01, 0x00, 0xc8, 0xe0, 0x02, 0x01, 0xf4, 0xe0, 0x05, 0x03,
    ];
    let page1 = [
        0xe8, 0xe0, 0x0a, 0x07, 0xd0, 0xe0, 0x14, 0x13, 0x88, 0x00, 0x00, 0x00, 0x00,
    ];

    // The first page alone has no terminator, forcing a second page fetch.
    assert!(terminated_word_len(&page0).is_none());

    let mut stream = page0.to_vec();
    stream.extend_from_slice(&page1);
    assert!(terminated_word_len(&stream).is_some());

    assert_eq!(
        parse_dpi_ranges(&stream).unwrap(),
        [
            DpiRange::Stepped {
                from: 100,
                to: 200,
                step: 1
            },
            DpiRange::Stepped {
                from: 200,
                to: 500,
                step: 2
            },
            DpiRange::Stepped {
                from: 500,
                to: 1000,
                step: 5
            },
            DpiRange::Stepped {
                from: 1000,
                to: 2000,
                step: 10
            },
            DpiRange::Stepped {
                from: 2000,
                to: 5000,
                step: 20
            },
        ]
    );
}

#[test]
fn parses_range_followed_by_fixed_value() {
    // A stepped range whose endpoint is not reused by the next entry must not be
    // re-emitted as a fixed value.
    let stream = [0x00, 0x64, 0xe0, 0x01, 0x00, 0xc8, 0x01, 0x90, 0x00, 0x00];

    assert_eq!(
        parse_dpi_ranges(&stream).unwrap(),
        [
            DpiRange::Stepped {
                from: 100,
                to: 200,
                step: 1
            },
            DpiRange::Fixed(400),
        ]
    );
}

#[test]
fn rejects_hyphen_without_preceding_value() {
    let stream = [0xe0, 0x01, 0x01, 0x90, 0x00, 0x00];

    assert_matches!(
        parse_dpi_ranges(&stream),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn rejects_hyphen_without_following_value() {
    let stream = [0x01, 0x90, 0xe0, 0x01, 0x00, 0x00];

    assert_matches!(
        parse_dpi_ranges(&stream),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn rejects_zero_step_unused_marker() {
    // 0xe000 is the documented "unused" marker (step 0); it is not a valid range.
    let stream = [0x01, 0x90, 0xe0, 0x00, 0x04, 0xb0, 0x00, 0x00];

    assert_matches!(
        parse_dpi_ranges(&stream),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn rejects_descending_stepped_range() {
    let stream = [0x06, 0x40, 0xe0, 0x32, 0x01, 0x90, 0x00, 0x00];

    assert_matches!(
        parse_dpi_ranges(&stream),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn rejects_unterminated_dpi_ranges() {
    let stream = [0x01, 0x90, 0x03, 0x20];

    assert_matches!(
        parse_dpi_ranges(&stream),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn parses_dpi_list_with_terminator() {
    // Spec example: a profile configured to 400, 800 and 1600 DPI.
    let bytes = [0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0x00, 0x00];

    assert_eq!(parse_dpi_list(&bytes).unwrap(), [400, 800, 1600]);
}

#[test]
fn parses_dpi_list_filling_payload() {
    // A full list leaves no room for the terminator.
    let bytes = [0x01, 0x90, 0x03, 0x20];

    assert_eq!(parse_dpi_list(&bytes).unwrap(), [400, 800]);
}

#[test]
fn parses_lod_list() {
    let bytes = [1, 2, 3, 0, 0, 0];

    assert_eq!(
        parse_lod_list(&bytes, 3).unwrap(),
        [Lod::Low, Lod::Medium, Lod::High]
    );
}

#[test]
fn rejects_unknown_lod_value() {
    let bytes = [9];

    assert_matches!(
        parse_lod_list(&bytes, 1),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn rejects_lod_list_longer_than_payload() {
    let bytes = [1, 2];

    assert_matches!(
        parse_lod_list(&bytes, 3),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn decodes_parameters_changed_event() {
    let mut payload = [0; 16];
    payload[0] = 1;
    payload[1..3].copy_from_slice(&800u16.to_be_bytes());
    payload[3..5].copy_from_slice(&1600u16.to_be_bytes());
    payload[5] = 2;

    let ExtendedDpiEvent::ParametersChanged(event) = decode_event(0, &payload).unwrap() else {
        panic!("expected a parameters-changed event");
    };
    assert_eq!(event.sensor_index, 1);
    assert_eq!(event.dpi_x, 800);
    assert_eq!(event.dpi_y, 1600);
    assert_eq!(event.lod, Some(Lod::Medium));
}

#[test]
fn decodes_calibration_completed_event() {
    let mut payload = [0; 16];
    payload[1] = 1;
    payload[2..4].copy_from_slice(&100i16.to_be_bytes());
    payload[4..6].copy_from_slice(&(-1i16).to_be_bytes());

    let ExtendedDpiEvent::CalibrationCompleted(event) = decode_event(1, &payload).unwrap() else {
        panic!("expected a calibration-completed event");
    };
    assert_eq!(event.direction, DpiDirection::Y);
    assert_eq!(event.correction, 100);
    assert_eq!(event.delta, -1);
    assert!(!event.failed());
}

#[test]
fn flags_failed_calibration_event() {
    let mut payload = [0; 16];
    payload[2..4].copy_from_slice(&i16::MIN.to_be_bytes());

    let ExtendedDpiEvent::CalibrationCompleted(event) = decode_event(1, &payload).unwrap() else {
        panic!("expected a calibration-completed event");
    };
    assert!(event.failed());
}

#[test]
fn ignores_unknown_event_sub_id() {
    assert!(decode_event(7, &[0; 16]).is_none());
}

#[test]
fn keeps_event_with_unknown_lod() {
    let mut payload = [0; 16];
    payload[0] = 3; // sensor index — a valid sibling that must survive
    payload[5] = 9;

    let ExtendedDpiEvent::ParametersChanged(changed) =
        decode_event(0, &payload).expect("event kept")
    else {
        panic!("expected ParametersChanged");
    };
    // Lod stays a closed, write-safe enum; an unknown lift-off value surfaces
    // as None on the event without dropping the DPI change.
    assert_eq!(changed.lod, None);
    assert_eq!(changed.sensor_index, 3);
}

/// Totality: a known event sub-id must decode for any enum-field byte value.
#[test]
fn known_sub_id_survives_any_field_byte() {
    for byte in 0..=u8::MAX {
        // sub 0: Lod at payload[5]; sub 1: DpiDirection at payload[1].
        for (sub_id, pos) in [(0u8, 5usize), (1, 1)] {
            let mut payload = [0; 16];
            payload[0] = 3; // sensor index sibling
            payload[pos] = byte;
            assert!(
                decode_event(sub_id, &payload).is_some(),
                "dropped sub {sub_id} for payload[{pos}]={byte:#04x}"
            );
        }
    }
}

#[test]
fn encodes_calibration_correction_sentinels() {
    assert_eq!(
        DpiCalibrationCorrection::Adjust(100).to_wire().unwrap(),
        100
    );
    assert_eq!(
        DpiCalibrationCorrection::Adjust(-512).to_wire().unwrap(),
        -512
    );
    assert_eq!(DpiCalibrationCorrection::RevertToOob.to_wire().unwrap(), 0);
    assert_eq!(
        DpiCalibrationCorrection::RevertToProfile.to_wire().unwrap(),
        i16::MIN
    );
}

#[test]
fn rejects_out_of_range_calibration_corrections() {
    for correction in [
        DpiCalibrationCorrection::Adjust(-1024),
        DpiCalibrationCorrection::Adjust(1024),
        DpiCalibrationCorrection::Adjust(i16::MIN),
    ] {
        assert_matches!(
            correction.to_wire(),
            Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
        );
    }
}
