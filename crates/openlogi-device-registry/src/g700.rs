//! Logitech G700 / G700s gaming mouse identity.
//!
//! A HID++ 1.0 mouse with five onboard profiles. Wireless, it pairs with its
//! own Nano receiver (also in [`crate::receiver`]) and answers there at device
//! index 1; over its charging cable it enumerates as a wired USB device. The
//! ids and the index follow libratbag's `data/devices/logitech-g700*.device`
//! (MIT) and Solaar. That receiver refuses the pairing-information register
//! (`0xB5`), so the slot cannot be looked up by product id there.

/// The wireless product id (WPID) the receiver reports for the G700.
pub const G700_WIRELESS_PRODUCT_ID: u16 = 0x1023;
/// The USB product id of the G700 on its cable.
pub const G700_WIRED_PRODUCT_ID: u16 = 0xc06b;
/// The USB product id of the G700's own Nano receiver.
pub const G700_RECEIVER_PRODUCT_ID: u16 = 0xc531;
/// The receiver slot (HID++ device index) the G700 answers at behind its
/// receiver.
pub const G700_RECEIVER_SLOT: u8 = 1;
