//! Unit tests for `LockKeyState` broadcast decoding.

use super::{LockKeyStateFeature, LockKeys};

fn report(first: u8) -> [u8; 16] {
    let mut payload = [0xff; 16];
    payload[0] = first;
    payload
}

#[test]
fn reads_each_lock_key_from_its_bit() {
    for (byte, keys) in [
        (0x00, LockKeys::empty()),
        (0x01, LockKeys::NUM_LOCK),
        (0x02, LockKeys::CAPS_LOCK),
        (0x04, LockKeys::SCROLL_LOCK),
        (0x03, LockKeys::NUM_LOCK | LockKeys::CAPS_LOCK),
    ] {
        assert_eq!(LockKeyStateFeature::decode_broadcast(&report(byte)), keys);
    }
}

#[test]
fn only_the_first_byte_carries_the_state() {
    // `report` fills every other byte with 0xff.
    assert_eq!(
        LockKeyStateFeature::decode_broadcast(&report(0x02)),
        LockKeys::CAPS_LOCK
    );
}

#[test]
fn an_unnamed_bit_is_kept() {
    let keys = LockKeyStateFeature::decode_broadcast(&report(0x82));
    assert!(keys.contains(LockKeys::CAPS_LOCK));
    assert_eq!(keys.bits(), 0x82);
}
