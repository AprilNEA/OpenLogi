use super::types::{OnboardMode, ProfileDirectoryEntry, ProfilesDescription, parse_directory};
use crate::protocol::v20::Hidpp20Error;

#[test]
fn parses_full_description_payload() {
    // G502-style description: 5 user profiles, 3 OOB, 11 buttons, 16 sectors
    // of 256 bytes (0x0100 big-endian at offset 7).
    let payload = [1, 2, 3, 5, 3, 11, 16, 0x01, 0x00, 0x04, 0x07, 0, 0, 0, 0, 0];

    let descr = ProfilesDescription::from_payload(&payload);

    assert_eq!(descr.memory_model_id, 1);
    assert_eq!(descr.profile_format_id, 2);
    assert_eq!(descr.macro_format_id, 3);
    assert_eq!(descr.profile_count, 5);
    assert_eq!(descr.profile_count_oob, 3);
    assert_eq!(descr.button_count, 11);
    assert_eq!(descr.sector_count, 16);
    assert_eq!(descr.sector_size, 256);
    assert_eq!(descr.mechanical_layout, 0x04);
    assert_eq!(descr.various_info, 0x07);
}

#[test]
fn onboard_mode_roundtrips_known_values() {
    assert_eq!(OnboardMode::try_from(1), Ok(OnboardMode::Onboard));
    assert_eq!(OnboardMode::try_from(2), Ok(OnboardMode::Host));
    assert_eq!(u8::from(OnboardMode::Onboard), 1);
    assert_eq!(u8::from(OnboardMode::Host), 2);
}

#[test]
fn rejects_unknown_mode_discriminants() {
    // 0 is "no change" in set requests and never a valid reported mode.
    assert!(OnboardMode::try_from(0).is_err());
    assert!(OnboardMode::try_from(3).is_err());
}

#[test]
fn parse_directory_stops_at_terminator() {
    let bytes = [
        0x00, 0x01, 0x01, 0x00, // sector 1, enabled
        0x00, 0x02, 0x00, 0x00, // sector 2, disabled
        0xff, 0xff, 0xff, 0xff, // terminator
        0x00, 0x03, 0x01, 0x00, // past the terminator, must be ignored
    ];

    let entries = parse_directory(&bytes, 5).expect("directory should parse");

    assert_eq!(
        entries,
        vec![
            ProfileDirectoryEntry {
                sector: 1,
                enabled: true
            },
            ProfileDirectoryEntry {
                sector: 2,
                enabled: false
            },
        ]
    );
}

#[test]
fn parse_directory_handles_erased_flash() {
    // A never-written directory reads back as erased flash.
    let entries = parse_directory(&[0xff; 16], 5).expect("erased flash should parse");

    assert!(entries.is_empty());
}

#[test]
fn parse_directory_respects_max_entries() {
    // No terminator within the bound: a full directory simply fills up.
    let bytes = [
        0x00, 0x01, 0x01, 0x00, //
        0x00, 0x02, 0x01, 0x00, //
        0x00, 0x03, 0x01, 0x00, //
    ];

    let entries = parse_directory(&bytes, 2).expect("bounded parse should succeed");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].sector, 2);
}

#[test]
fn parse_directory_rejects_unknown_enabled_byte() {
    let bytes = [0x00, 0x01, 0x02, 0x00];

    assert!(matches!(
        parse_directory(&bytes, 5),
        Err(Hidpp20Error::UnsupportedResponse)
    ));
}

#[test]
fn parse_directory_rejects_truncated_entry() {
    // Two full entries, then a 2-byte tail with neither terminator nor bound.
    let bytes = [
        0x00, 0x01, 0x01, 0x00, //
        0x00, 0x02, 0x01, 0x00, //
        0x00, 0x03,
    ];

    assert!(matches!(
        parse_directory(&bytes, 5),
        Err(Hidpp20Error::UnsupportedResponse)
    ));
}

#[test]
fn parse_directory_accepts_rom_sectors() {
    let bytes = [
        0x01, 0x01, 0x01, 0x00, // ROM profile 1
        0xff, 0xff, 0xff, 0xff,
    ];

    let entries = parse_directory(&bytes, 5).expect("ROM entry should parse");

    assert_eq!(
        entries,
        vec![ProfileDirectoryEntry {
            sector: 0x0101,
            enabled: true
        }]
    );
}
