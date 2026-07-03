use super::*;

#[test]
fn passkey_clicks_are_msb_first_10_bits() {
    // 0b00_0000_0101 = 5 -> eight lefts then right, left, right.
    assert_eq!(
        passkey_to_clicks("5"),
        vec![
            Click::Left,
            Click::Left,
            Click::Left,
            Click::Left,
            Click::Left,
            Click::Left,
            Click::Left,
            Click::Right,
            Click::Left,
            Click::Right,
        ]
    );
}
