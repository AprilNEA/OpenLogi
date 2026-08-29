//! Logitech G502-family hardware identities.

/// USB, Bluetooth, and E-Quad model identifiers used by G502-family mice.
pub const MODEL_IDS: &[u16] = &[
    0x407e, 0x407f, 0x4099, 0x409a, 0xc08b, 0xc090, 0xc091, 0xc095, 0xc098, 0xc09d, 0xc332,
];

/// Whether `model_id` belongs to the G502 family.
#[must_use]
pub fn is_model_id(model_id: u16) -> bool {
    model_id != 0 && MODEL_IDS.contains(&model_id)
}

/// Whether a device marketing name identifies the G502 family.
///
/// Logitech names vary in case and occasionally include punctuation between
/// the family letter and number, so ASCII punctuation is ignored.
#[must_use]
pub fn is_marketing_name(name: &str) -> bool {
    const FAMILY: &[u8] = b"g502";
    let mut matched = 0;
    for byte in name
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
    {
        matched = if byte == FAMILY[matched] {
            matched + 1
        } else {
            usize::from(byte == FAMILY[0])
        };
        if matched == FAMILY.len() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_marketing_name, is_model_id};

    #[test]
    fn recognizes_known_model_ids() {
        assert!(is_model_id(0x4099));
        assert!(is_model_id(0xc095));
        assert!(!is_model_id(0));
        assert!(!is_model_id(0xb034));
    }

    #[test]
    fn recognizes_marketing_name_variants() {
        assert!(is_marketing_name("Logitech G502 X Plus"));
        assert!(is_marketing_name("G-502 LIGHTSPEED"));
        assert!(!is_marketing_name("MX Master 3S"));
    }
}
