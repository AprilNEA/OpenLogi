//! Unit tests for `Equalizer` payload parsing, using the spec's worked example
//! (10 bands at ±12 dB).

use super::{
    EqCapabilities, EqInfo, GainLocation, GainPersistence, parse_frequency_page, parse_gains,
};

#[test]
fn parses_eq_info_with_implied_range() {
    // Spec example: bandCount = 10, dbRange = 0x0c, the rest zero.
    let mut payload = [0; 16];
    payload[0] = 0x0a;
    payload[1] = 0x0c;

    let info = EqInfo::from_payload(&payload);
    assert_eq!(info.band_count, 10);
    assert_eq!(info.db_range, 12);
    assert_eq!(info.capabilities, EqCapabilities::empty());
    assert_eq!(info.db_min, 0);
    assert_eq!(info.db_max, 0);
    // Both bounds zero ⇒ ±dbRange.
    assert_eq!(info.effective_range(), (-12, 12));
}

#[test]
fn parses_eq_info_with_explicit_range() {
    let mut payload = [0; 16];
    payload[0] = 5;
    payload[1] = 0x0c;
    payload[2] = EqCapabilities::STORED_AS_GAINS.bits();
    payload[3] = (-6i8) as u8;
    payload[4] = 9;

    let info = EqInfo::from_payload(&payload);
    assert!(info.capabilities.contains(EqCapabilities::STORED_AS_GAINS));
    assert_eq!(info.db_min, -6);
    assert_eq!(info.db_max, 9);
    assert_eq!(info.effective_range(), (-6, 9));
}

#[test]
fn parses_first_frequency_page() {
    // Spec example: getFrequencies(0) → 32, 64, 125, 250, 500, 1000, 2000 Hz.
    let mut payload = [0; 16];
    payload[0] = 0; // echoed band index
    let freqs = [32u16, 64, 125, 250, 500, 1000, 2000];
    for (i, f) in freqs.iter().enumerate() {
        payload[1 + 2 * i..3 + 2 * i].copy_from_slice(&f.to_be_bytes());
    }

    assert_eq!(parse_frequency_page(&payload, 7).unwrap(), freqs);
}

#[test]
fn parses_partial_frequency_page() {
    // Spec example: getFrequencies(7) → 4000, 8000, 16000 Hz.
    let mut payload = [0; 16];
    payload[0] = 7;
    let freqs = [4000u16, 8000, 16000];
    for (i, f) in freqs.iter().enumerate() {
        payload[1 + 2 * i..3 + 2 * i].copy_from_slice(&f.to_be_bytes());
    }

    assert_eq!(parse_frequency_page(&payload, 3).unwrap(), freqs);
}

#[test]
fn rejects_oversized_frequency_page() {
    // A page can hold at most seven u16 values after the index byte.
    assert!(parse_frequency_page(&[0; 16], 8).is_err());
}

#[test]
fn parses_gains_from_response() {
    // Spec example: gains {0, -12, 12, 0, ...}; -12 = 0xF4, +12 = 0x0C.
    let mut payload = [0; 16];
    payload[0] = 0x00;
    payload[1] = 0xf4;
    payload[2] = 0x0c;

    assert_eq!(
        parse_gains(&payload, 0, 10).unwrap(),
        [0, -12, 12, 0, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn parses_echoed_gains_after_persistence_byte() {
    // setFrequencyGains echoes persistence@0 then the gains, so reading from
    // offset 1 recovers them.
    let mut payload = [0; 16];
    payload[0] = 1; // echoed persistence
    payload[1] = (-4i8) as u8;
    payload[2] = 4;

    assert_eq!(parse_gains(&payload, 1, 3).unwrap(), [-4, 4, 0]);
}

#[test]
fn rejects_too_many_gains() {
    assert!(parse_gains(&[0; 16], 1, 16).is_err());
}

#[test]
fn maps_enum_wire_values() {
    assert_eq!(u8::from(GainLocation::Ram), 1);
    assert_eq!(GainLocation::try_from(0u8).unwrap(), GainLocation::Eeprom);
    assert_eq!(u8::from(GainPersistence::NonVolatileOnly), 2);
    assert!(GainPersistence::try_from(3u8).is_err());
}
