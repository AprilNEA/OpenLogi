//! Logitech G600 MMO gaming mouse raw-HID identity.
//!
//! The G600 predates HID++ 2.0 on gaming mice and answers no HID++ request.
//! Its configuration lives behind vendor feature reports on the second USB
//! interface's `0xff80/0x0080` collection (the first collection there is the
//! G-key keyboard): report `0xf0` carries the active profile and DPI slot,
//! `0xf3`–`0xf5` the three 154-byte onboard profiles (libratbag,
//! `src/driver-logitech-g600.c`, MIT). OpenLogi surfaces it as a standalone
//! raw-HID device for identity only; no report is read or written yet.

use crate::LOGITECH_VENDOR_ID;

/// Stable driver-family identifier carried by standalone G600 inventory.
pub const G600_DRIVER_ID: &str = "logitech-g600";
/// G600 USB product ID.
pub const G600_PRODUCT_ID: u16 = 0xc24a;
/// Usage page of the G600's vendor feature-report collection.
pub const G600_USAGE_PAGE: u16 = 0xff80;
/// Usage ID of the G600's vendor feature-report collection.
pub const G600_USAGE_ID: u16 = 0x0080;
/// Marketed product name.
pub const G600_NAME: &str = "G600 Gaming Mouse";

/// Whether an HID node is the G600's vendor feature-report collection.
#[must_use]
pub fn matches_g600(vendor_id: u16, product_id: u16, usage_page: u16, usage_id: u16) -> bool {
    (vendor_id, product_id, usage_page, usage_id)
        == (
            LOGITECH_VENDOR_ID,
            G600_PRODUCT_ID,
            G600_USAGE_PAGE,
            G600_USAGE_ID,
        )
}

#[cfg(test)]
mod tests {
    use super::matches_g600;
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn vendor_collection_matches() {
        assert!(matches_g600(LOGITECH_VENDOR_ID, 0xc24a, 0xff80, 0x0080));
    }

    #[test]
    fn sibling_collections_and_other_vendors_do_not_match() {
        // Mouse interface and the G-key keyboard collection of the same device.
        assert!(!matches_g600(LOGITECH_VENDOR_ID, 0xc24a, 0x0001, 0x0002));
        assert!(!matches_g600(LOGITECH_VENDOR_ID, 0xc24a, 0x0001, 0x0006));
        assert!(!matches_g600(0xffff, 0xc24a, 0xff80, 0x0080));
    }
}
