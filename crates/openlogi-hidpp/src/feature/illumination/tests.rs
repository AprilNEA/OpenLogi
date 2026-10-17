//! Unit tests for `Illumination` payload parsing and event decoding.

use std::assert_matches;

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
    let [payload] = SetLevels::Reset.to_payloads().unwrap()[..] else {
        panic!("reset is one request");
    };
    assert_eq!(payload[0], 1 << 1);
    assert!(payload[1..].iter().all(|&b| b == 0));
}

#[test]
fn encodes_linear_levels() {
    let pages = SetLevels::Linear {
        min: 100,
        max: 500,
        step: 50,
    }
    .to_payloads()
    .unwrap();
    let [payload] = pages[..] else {
        panic!("linear levels are one request");
    };
    assert_eq!(payload[0], 1);
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 100);
    assert_eq!(u16::from_be_bytes([payload[4], payload[5]]), 500);
    assert_eq!(u16::from_be_bytes([payload[6], payload[7]]), 50);
}

#[test]
fn encodes_non_linear_levels() {
    let pages = SetLevels::NonLinear {
        values: vec![100, 200],
    }
    .to_payloads()
    .unwrap();
    let [payload] = pages[..] else {
        panic!("two levels fit in one request");
    };

    assert_eq!(payload[0], 2 << 5); // validCount = 2, linear/reset clear
    assert_eq!(payload[1], 2); // startIndex 0, levelCount 2
    assert_eq!(u16::from_be_bytes([payload[2], payload[3]]), 100);
    assert_eq!(u16::from_be_bytes([payload[4], payload[5]]), 200);
}

#[test]
fn pages_a_full_non_linear_table_with_one_level_count() {
    // 15 levels need three requests (7 + 7 + 1). Every page names its own
    // start index and the same total, so the device sees one consistent table.
    let values: Vec<u16> = (1..=15).map(|i| i * 100).collect();
    let pages = SetLevels::NonLinear {
        values: values.clone(),
    }
    .to_payloads()
    .unwrap();

    let headers: Vec<[u8; 2]> = pages.iter().map(|page| [page[0], page[1]]).collect();
    assert_eq!(
        headers,
        [
            [7 << 5, 15],
            [7 << 5, (7 << 4) | 15],
            [1 << 5, (14 << 4) | 15]
        ]
    );
    // The last page carries one value; the rest of its payload stays zero.
    assert!(pages[2][4..].iter().all(|&b| b == 0));

    // Each page decodes as the matching slice of the table, so reading the
    // table back page by page yields exactly what was written.
    let mut read_back = Vec::new();
    for (page, expected_start) in pages.iter().zip([0, 7, 14]) {
        let LevelConfig::NonLinear {
            start_index,
            level_count,
            values,
        } = LevelConfig::from_payload(page)
        else {
            panic!("non-linear pages decode as non-linear");
        };
        assert_eq!((start_index, level_count), (expected_start, 15));
        read_back.extend(values);
    }
    assert_eq!(read_back, values);
}

#[test]
fn rejects_non_linear_tables_the_count_field_cannot_hold() {
    for values in [vec![], (1..=16).collect::<Vec<u16>>()] {
        assert_matches!(
            SetLevels::NonLinear { values }.to_payloads(),
            Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
        );
    }
}

#[test]
fn writes_every_page_of_a_non_linear_table() {
    use std::sync::Arc;

    use super::{FN_SET_BRIGHTNESS_LEVELS, IlluminationFeature};
    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::{Message, MessageHeader};

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let feature = IlluminationFeature::new(channel, 3, 8);
        let header = MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: U4::from_lo(FN_SET_BRIGHTNESS_LEVELS),
            software_id: U4::from_lo(1),
        };
        for _ in 0..2 {
            handle.queue_response(Message::Long(header, [0; 16]).into());
        }

        let values: Vec<u16> = (1..=9).map(|i| i * 10).collect();
        feature
            .set_brightness_levels(&SetLevels::NonLinear { values })
            .await
            .unwrap();

        // Report: [id, device, feature, function|swid, payload...].
        let headers: Vec<[u8; 2]> = handle
            .written_reports()
            .iter()
            .map(|report| [report[4], report[5]])
            .collect();
        assert_eq!(headers, [[7 << 5, 9], [2 << 5, (7 << 4) | 9]]);
    });
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

/// Totality: the clamp-source event must decode for any source byte; an
/// unknown source folds to `Other(_)` and the brightness sibling survives.
#[test]
fn keeps_clamp_event_with_unknown_source() {
    for byte in 0..=u8::MAX {
        let mut payload = [0; 16];
        payload[0] = byte; // BrightnessClampedSource
        payload[1..3].copy_from_slice(&300u16.to_be_bytes()); // brightness sibling
        let event = decode_event(4, &payload)
            .unwrap_or_else(|| panic!("dropped clamp event for source {byte:#04x}"));
        assert_eq!(
            event,
            IlluminationEvent::BrightnessClamped {
                source: BrightnessClampedSource::from(byte),
                brightness: 300,
            }
        );
    }
}

/// The reply a reader gets when the device answers function `function`
/// with `reply`.
fn read_with<T, F>(
    function: u8,
    read: impl FnOnce(super::IlluminationFeature) -> F,
    reply: impl FnOnce(crate::protocol::v20::MessageHeader) -> crate::protocol::v20::Message,
) -> Result<T, Hidpp20Error>
where
    F: Future<Output = Result<T, Hidpp20Error>>,
{
    use std::sync::Arc;

    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::MessageHeader;

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let mut pending = Box::pin(read(super::IlluminationFeature::new(channel, 3, 8)));
        assert!(futures::poll!(pending.as_mut()).is_pending());
        let header = MessageHeader {
            device_index: 3,
            feature_index: 8,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        };
        handle.send_incoming(reply(header).into()).await;
        pending.await
    })
}

#[test]
fn info_and_level_reads_refuse_a_short_reply() {
    use super::{
        FN_GET_BRIGHTNESS, FN_GET_BRIGHTNESS_INFO, FN_GET_BRIGHTNESS_LEVELS,
        FN_GET_COLOR_TEMPERATURE_INFO, FN_GET_COLOR_TEMPERATURE_LEVELS, IlluminationFeature,
        LevelConfig,
    };
    use crate::protocol::v20::Message;

    // A short reply that, zero-filled, would decode as a real answer: a
    // linear-levels range with min 100, or a linear level page.
    let short = |header| Message::Short(header, [0b0000_0010, 0, 100]);
    let mut long_info = [0; 16];
    long_info[..8].copy_from_slice(&[0b0000_0010, 0, 100, 0x03, 0xe8, 0, 10, 5]);

    for function in [FN_GET_BRIGHTNESS_INFO, FN_GET_COLOR_TEMPERATURE_INFO] {
        let info = |f: IlluminationFeature| async move {
            if function == FN_GET_BRIGHTNESS_INFO {
                f.get_brightness_info().await
            } else {
                f.get_color_temperature_info().await
            }
        };
        assert_matches!(
            read_with(function, info, short),
            Err(Hidpp20Error::UnsupportedResponse)
        );
        let parsed = read_with(function, info, |header| Message::Long(header, long_info)).unwrap();
        assert_eq!((parsed.min, parsed.max, parsed.max_levels), (100, 1000, 5));
    }

    for function in [FN_GET_BRIGHTNESS_LEVELS, FN_GET_COLOR_TEMPERATURE_LEVELS] {
        let levels = |f: IlluminationFeature| async move {
            if function == FN_GET_BRIGHTNESS_LEVELS {
                f.get_brightness_levels(0).await
            } else {
                f.get_color_temperature_levels(0).await
            }
        };
        let short_linear = |header| Message::Short(header, [1, 0, 0]);
        assert_matches!(
            read_with(function, levels, short_linear),
            Err(Hidpp20Error::UnsupportedResponse)
        );
        // A long page still decodes: three of six non-linear levels from 1.
        let mut page = [0; 16];
        page[0] = 3 << 5;
        page[1] = (1 << 4) | 6;
        page[2..4].copy_from_slice(&200u16.to_be_bytes());
        page[4..6].copy_from_slice(&400u16.to_be_bytes());
        page[6..8].copy_from_slice(&800u16.to_be_bytes());
        assert_eq!(
            read_with(function, levels, |header| Message::Long(header, page)).unwrap(),
            LevelConfig::NonLinear {
                start_index: 1,
                level_count: 6,
                values: vec![200, 400, 800],
            }
        );
    }

    // A value fits a short reply and is still read from one.
    let brightness = |f: IlluminationFeature| async move { f.get_brightness().await };
    assert_eq!(
        read_with(FN_GET_BRIGHTNESS, brightness, |header| {
            Message::Short(header, [0x01, 0x2c, 0])
        })
        .unwrap(),
        300
    );
}
