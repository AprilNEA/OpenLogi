//! Logitech LIGHTSPEED gaming receivers.
//!
//! LIGHTSPEED receivers expose the same HID++ 1.0 receiver registers used by
//! Unifying-style enumeration, but have their own USB product IDs and are not
//! Logi Bolt receivers.

/// All USB vendor & product ID pairs that are known to identify LIGHTSPEED
/// gaming receivers.
pub const VPID_PAIRS: &[(u16, u16)] = &[
    (0x046d, 0xc539),
    (0x046d, 0xc53a),
    (0x046d, 0xc53d),
    (0x046d, 0xc53f),
    (0x046d, 0xc541),
    (0x046d, 0xc545),
    (0x046d, 0xc547),
    (0x046d, 0xc54d),
];
