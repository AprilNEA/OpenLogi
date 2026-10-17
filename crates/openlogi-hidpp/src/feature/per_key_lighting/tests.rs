//! Unit tests for `PerKeyLighting` request encoding.

use std::assert_matches;

use super::{
    FramePersistence, Rgb, RgbZone, RgbZoneRange, ZonePresencePage, consecutive_zones_args,
    delta_args, frame_end_args, individual_zones_args, range_zones_args, single_value_args,
    validate_individual_zones, validate_ranges, validate_single_value_zones, validate_zone_id,
};
use crate::protocol::v20::{ErrorType, Hidpp20Error};

const RED: Rgb = Rgb {
    red: 0xff,
    green: 0,
    blue: 0,
};

#[test]
fn encodes_individual_zones() {
    let args = individual_zones_args(&[
        RgbZone {
            zone_id: 5,
            color: RED,
        },
        RgbZone {
            zone_id: 9,
            color: Rgb {
                red: 1,
                green: 2,
                blue: 3,
            },
        },
    ]);
    assert_eq!(args[0..4], [5, 0xff, 0, 0]);
    assert_eq!(args[4..8], [9, 1, 2, 3]);
    // Unused slots stay zero (the zone-id sentinel).
    assert_eq!(args[8..16], [0; 8]);
}

#[test]
fn individual_zones_caps_at_four() {
    let zones = [RgbZone {
        zone_id: 1,
        color: RED,
    }; 6];
    let args = individual_zones_args(&zones);
    // Only four slots (16 bytes) are produced; the 5th/6th are dropped.
    assert_eq!(args[12..16], [1, 0xff, 0, 0]);
}

#[test]
fn rejects_reserved_zone_ids() {
    for zone_id in [0, 0xff] {
        assert_matches!(
            validate_zone_id(zone_id),
            Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
        );
    }

    assert_matches!(
        validate_individual_zones(&[RgbZone {
            zone_id: 0,
            color: RED,
        }]),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    );
    assert_matches!(
        validate_ranges(&[RgbZoneRange {
            first_zone_id: 1,
            last_zone_id: 0xff,
            color: RED,
        }]),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    );
    assert_matches!(
        validate_single_value_zones(&[1, 0xff]),
        Err(Hidpp20Error::Feature(ErrorType::InvalidArgument))
    );
}

#[test]
fn encodes_consecutive_zones() {
    let colors = [
        Rgb {
            red: 1,
            green: 2,
            blue: 3,
        },
        Rgb {
            red: 4,
            green: 5,
            blue: 6,
        },
        Rgb {
            red: 7,
            green: 8,
            blue: 9,
        },
        Rgb {
            red: 10,
            green: 11,
            blue: 12,
        },
        Rgb {
            red: 13,
            green: 14,
            blue: 15,
        },
    ];
    let args = consecutive_zones_args(20, colors);
    assert_eq!(args[0], 20);
    assert_eq!(args[1..4], [1, 2, 3]);
    assert_eq!(args[13..16], [13, 14, 15]);
}

#[test]
fn encodes_range_zones() {
    let args = range_zones_args(&[
        RgbZoneRange {
            first_zone_id: 1,
            last_zone_id: 5,
            color: RED,
        },
        RgbZoneRange {
            first_zone_id: 10,
            last_zone_id: 12,
            color: Rgb {
                red: 0,
                green: 0xff,
                blue: 0,
            },
        },
    ]);
    assert_eq!(args[0..5], [1, 5, 0xff, 0, 0]);
    assert_eq!(args[5..10], [10, 12, 0, 0xff, 0]);
}

#[test]
fn encodes_single_value_zones() {
    let args = single_value_args(
        Rgb {
            red: 0x10,
            green: 0x20,
            blue: 0x30,
        },
        &[1, 2, 3],
    );
    assert_eq!(args[0..3], [0x10, 0x20, 0x30]);
    assert_eq!(args[3..6], [1, 2, 3]);
    assert_eq!(args[6], 0);
}

#[test]
fn encodes_frame_end_big_endian() {
    let args = frame_end_args(FramePersistence::VolatileAndNonVolatile, 0x0102, 0x0304);
    assert_eq!(args[0], 1);
    assert_eq!(args[1..3], [0x01, 0x02]);
    assert_eq!(args[3..5], [0x03, 0x04]);
}

#[test]
fn encodes_delta_payload_verbatim() {
    let packed = [0xaa; 15];
    let args = delta_args(7, packed);
    assert_eq!(args[0], 7);
    assert_eq!(args[1..16], [0xaa; 15]);
}

#[test]
fn maps_enum_wire_values() {
    assert_eq!(u8::from(ZonePresencePage::Zones112To223), 1);
    assert_eq!(
        ZonePresencePage::try_from(2u8).unwrap(),
        ZonePresencePage::Zones224To255
    );
    ZonePresencePage::try_from(3u8).unwrap_err();
    assert_eq!(u8::from(FramePersistence::VolatileAndNonVolatile), 1);
}

/// What a setter returns when the device answers it with `reply`, built
/// from the header the request went out with.
fn answered<F>(
    function: u8,
    call: impl FnOnce(super::PerKeyLightingFeature) -> F,
    reply: impl FnOnce(crate::protocol::v20::MessageHeader) -> crate::protocol::v20::Message,
) -> Result<(), Hidpp20Error>
where
    F: Future<Output = Result<(), Hidpp20Error>>,
{
    use std::sync::Arc;

    use crate::channel::mock::{MockRawHidChannel, channel_with_reader};
    use crate::feature::CreatableFeature;
    use crate::nibble::U4;
    use crate::protocol::v20::MessageHeader;

    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let mut pending = Box::pin(call(super::PerKeyLightingFeature::new(channel, 3, 7)));
        assert!(futures::poll!(pending.as_mut()).is_pending());
        let header = MessageHeader {
            device_index: 3,
            feature_index: 7,
            function_id: U4::from_lo(function),
            software_id: U4::from_lo(1),
        };
        handle.send_incoming(reply(header).into()).await;
        pending.await
    })
}

/// A long reply repeating `args`, with `edit` applied to it.
fn echo(
    args: [u8; 16],
    edit: impl FnOnce(&mut [u8; 16]),
) -> impl FnOnce(crate::protocol::v20::MessageHeader) -> crate::protocol::v20::Message {
    move |header| {
        let mut payload = args;
        edit(&mut payload);
        crate::protocol::v20::Message::Long(header, payload)
    }
}

const ZONES: [RgbZone; 2] = [
    RgbZone {
        zone_id: 5,
        color: RED,
    },
    RgbZone {
        zone_id: 9,
        color: RED,
    },
];

const RANGES: [RgbZoneRange; 2] = [
    RgbZoneRange {
        first_zone_id: 4,
        last_zone_id: 8,
        color: RED,
    },
    RgbZoneRange {
        first_zone_id: 20,
        last_zone_id: 30,
        color: RED,
    },
];

#[test]
fn zone_writes_succeed_only_on_an_echo_of_their_zones() {
    let unsupported = |result| assert_matches!(result, Err(Hidpp20Error::UnsupportedResponse));

    let individual = individual_zones_args(&ZONES);
    let set_individual =
        |f: super::PerKeyLightingFeature| async move { f.set_individual_rgb_zones(&ZONES).await };
    answered(1, set_individual, echo(individual, |_| {})).unwrap();
    // Only the slots that were sent are compared.
    answered(1, set_individual, echo(individual, |p| p[8] = 0x33)).unwrap();
    unsupported(answered(1, set_individual, echo(individual, |p| p[4] = 10)));

    let consecutive = consecutive_zones_args(5, [RED; 5]);
    let set_consecutive = |f: super::PerKeyLightingFeature| async move {
        f.set_consecutive_rgb_zones(5, [RED; 5]).await
    };
    answered(2, set_consecutive, echo(consecutive, |p| p[1] = 0)).unwrap();
    unsupported(answered(
        2,
        set_consecutive,
        echo(consecutive, |p| p[0] = 6),
    ));

    for function in [3, 4] {
        let delta = delta_args(5, [0x5a; 15]);
        let set_delta = |f: super::PerKeyLightingFeature| async move {
            if function == 3 {
                f.set_consecutive_rgb_zones_delta_5bit(5, [0x5a; 15]).await
            } else {
                f.set_consecutive_rgb_zones_delta_4bit(5, [0x5a; 15]).await
            }
        };
        answered(function, set_delta, echo(delta, |_| {})).unwrap();
        unsupported(answered(function, set_delta, echo(delta, |p| p[0] = 4)));
    }

    let ranges = range_zones_args(&RANGES);
    let set_ranges =
        |f: super::PerKeyLightingFeature| async move { f.set_range_rgb_zones(&RANGES).await };
    // Options+ checks later range starts at a stride the request layout does
    // not have, so only the first start is compared.
    answered(5, set_ranges, echo(ranges, |p| p[5] = 21)).unwrap();
    unsupported(answered(5, set_ranges, echo(ranges, |p| p[0] = 3)));

    let single = single_value_args(RED, &[5, 9]);
    let set_single = |f: super::PerKeyLightingFeature| async move {
        f.set_rgb_zones_single_value(RED, &[5, 9]).await
    };
    answered(6, set_single, echo(single, |_| {})).unwrap();
    unsupported(answered(6, set_single, echo(single, |p| p[3] = 6)));
}

#[test]
fn a_short_reply_to_a_zone_write_is_refused() {
    use crate::protocol::v20::Message;

    // Each short reply's first byte names the written zone, so only the
    // report length tells it from a real echo.
    let one = [ZONES[0]];
    let set_individual =
        |f: super::PerKeyLightingFeature| async move { f.set_individual_rgb_zones(&one).await };
    let set_consecutive = |f: super::PerKeyLightingFeature| async move {
        f.set_consecutive_rgb_zones(5, [RED; 5]).await
    };
    assert_matches!(
        answered(1, set_individual, |header| Message::Short(
            header,
            [5, 0xff, 0]
        )),
        Err(Hidpp20Error::UnsupportedResponse)
    );
    assert_matches!(
        answered(2, set_consecutive, |header| Message::Short(
            header,
            [5, 0xff, 0]
        )),
        Err(Hidpp20Error::UnsupportedResponse)
    );
}

#[test]
fn frame_end_takes_any_reply() {
    use crate::protocol::v20::Message;

    let end = |f: super::PerKeyLightingFeature| async move {
        f.frame_end(FramePersistence::Volatile, 0, 0).await
    };
    answered(7, end, |header| Message::Long(header, [0xee; 16])).unwrap();
}
