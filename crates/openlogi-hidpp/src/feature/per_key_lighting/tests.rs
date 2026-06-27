//! Unit tests for `PerKeyLighting` request encoding.

use super::{
    FramePersistence, Rgb, RgbZone, RgbZoneRange, ZonePresencePage, consecutive_zones_args,
    delta_args, frame_end_args, individual_zones_args, range_zones_args, single_value_args,
};

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
    assert!(ZonePresencePage::try_from(3u8).is_err());
    assert_eq!(u8::from(FramePersistence::VolatileAndNonVolatile), 1);
}
