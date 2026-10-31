//! Logitech G700 / G700s gaming mouse identity.
//!
//! A HID++ 1.0 mouse with five onboard profiles. Wireless, it pairs with its
//! own Nano receiver (also in [`crate::receiver`]) and answers there at device
//! index 1; over its charging cable it enumerates as a wired USB device. The
//! ids and the index follow libratbag's `data/devices/logitech-g700*.device`
//! (MIT) and Solaar. That receiver refuses the pairing-information register
//! (`0xB5`), so the slot cannot be looked up by product id there.
//!
//! [`g700_link`] is the one place that decides whether a HID++ node reaches a
//! G700: the inventory probe, the device layer's settings path and the CLI
//! diagnostics all ask it rather than comparing these ids themselves.

use crate::LOGITECH_VENDOR_ID;

/// The model name, for where no register names the mouse: its receiver
/// keeps no name for it.
pub const G700_NAME: &str = "G700";
/// The wireless product id (WPID) the receiver reports for the G700.
pub const G700_WIRELESS_PRODUCT_ID: u16 = 0x1023;
/// The USB product id of the G700 on its cable.
pub const G700_WIRED_PRODUCT_ID: u16 = 0xc06b;
/// The USB product id of the G700's own Nano receiver.
pub const G700_RECEIVER_PRODUCT_ID: u16 = 0xc531;
/// The receiver slot (HID++ device index) the G700 answers at behind its
/// receiver.
pub const G700_RECEIVER_SLOT: u8 = 1;
/// The HID++ device indexes a G700 on its cable may answer at, in the order
/// to try them: `0xFF`, the self index every wired HID++ device answers at,
/// then `0x00`, the index libratbag addresses a wired HID++ 1.0 mouse at
/// (`HIDPP_WIRED_DEVICE_IDX`). Neither is verified on the cable yet.
pub const G700_WIRED_DEVICE_INDEXES: [u8; 2] = [0xff, 0x00];

/// How a HID node reaches a G700.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum G700Link {
    /// The node is the G700's Nano receiver; the mouse is at
    /// [`G700_RECEIVER_SLOT`].
    Receiver,
    /// The node is the G700 itself, on its cable; it answers at one of
    /// [`G700_WIRED_DEVICE_INDEXES`].
    Cable,
}

impl G700Link {
    /// The HID++ device indexes the G700 may answer at on a node of this
    /// link, in the order to try them: the receiver's slot alone, or the
    /// cable's indexes.
    #[must_use]
    pub const fn device_indexes(self) -> &'static [u8] {
        match self {
            Self::Receiver => &[G700_RECEIVER_SLOT],
            Self::Cable => &G700_WIRED_DEVICE_INDEXES,
        }
    }

    /// Whether HID++ device `device_index` on a node of this link is the
    /// G700: the receiver's slot, or one of the cable's indexes.
    #[must_use]
    pub fn addresses_g700(self, device_index: u8) -> bool {
        self.device_indexes().contains(&device_index)
    }
}

/// How the HID node `vendor_id:product_id` reaches a G700, or `None` when it
/// is neither the mouse's receiver nor the mouse on its cable.
#[must_use]
pub fn g700_link(vendor_id: u16, product_id: u16) -> Option<G700Link> {
    match (vendor_id, product_id) {
        (LOGITECH_VENDOR_ID, G700_RECEIVER_PRODUCT_ID) => Some(G700Link::Receiver),
        (LOGITECH_VENDOR_ID, G700_WIRED_PRODUCT_ID) => Some(G700Link::Cable),
        _ => None,
    }
}

/// Whether HID++ device `device_index` on the node `vendor_id:product_id` is
/// a G700.
#[must_use]
pub fn is_g700(vendor_id: u16, product_id: u16, device_index: u8) -> bool {
    g700_link(vendor_id, product_id).is_some_and(|link| link.addresses_g700(device_index))
}

#[cfg(test)]
mod tests {
    use super::{G700Link, g700_link, is_g700};
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn the_receiver_and_the_cable_are_g700_links() {
        assert_eq!(
            g700_link(LOGITECH_VENDOR_ID, 0xc531),
            Some(G700Link::Receiver)
        );
        assert_eq!(g700_link(LOGITECH_VENDOR_ID, 0xc06b), Some(G700Link::Cable));
        assert_eq!(g700_link(LOGITECH_VENDOR_ID, 0xc52b), None, "Unifying");
        assert_eq!(g700_link(0xffff, 0xc531), None, "another vendor");
    }

    #[test]
    fn only_the_g700_slot_of_its_receiver_is_the_mouse() {
        assert!(is_g700(LOGITECH_VENDOR_ID, 0xc531, 1));
        assert!(!is_g700(LOGITECH_VENDOR_ID, 0xc531, 2));
        assert!(
            !is_g700(LOGITECH_VENDOR_ID, 0xc531, 0xff),
            "the receiver itself"
        );
    }

    #[test]
    fn the_cable_answers_at_either_wired_index() {
        assert!(is_g700(LOGITECH_VENDOR_ID, 0xc06b, 0xff));
        assert!(is_g700(LOGITECH_VENDOR_ID, 0xc06b, 0x00));
        assert!(!is_g700(LOGITECH_VENDOR_ID, 0xc06b, 1));
    }
}
