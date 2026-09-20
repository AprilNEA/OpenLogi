//! Logitech receiver identities.

use crate::LOGITECH_VENDOR_ID;

/// Logitech receiver product family, independent of its wire protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiverBrand {
    /// Logi Bolt receiver.
    Bolt,
    /// Logitech Unifying receiver.
    Unifying,
    /// Logitech Nano receiver.
    Nano,
    /// Logitech Lightspeed receiver.
    Lightspeed,
}

/// Receiver protocol implementation used to enumerate and address devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiverProtocol {
    /// Logi Bolt receiver registers and pairing flow.
    Bolt,
    /// HID++ 1.0 Unifying-compatible receiver registers and pairing flow.
    Unifying,
}

/// A known receiver's USB identity and the behavior OpenLogi assigns to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiverDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// Receiver's marketed product family.
    pub brand: ReceiverBrand,
    /// Protocol implementation used for receiver operations.
    pub protocol: ReceiverProtocol,
}

impl ReceiverDescriptor {
    const fn logitech(product_id: u16, brand: ReceiverBrand, protocol: ReceiverProtocol) -> Self {
        Self {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id,
            brand,
            protocol,
        }
    }
}

/// All receiver identities supported by OpenLogi.
///
/// Each entry is a protocol claim and must be backed by hardware verification
/// or an established reference implementation. Marketing families and protocol
/// families are recorded separately: Nano and Lightspeed currently use
/// [`ReceiverProtocol::Unifying`].
pub const RECEIVERS: &[ReceiverDescriptor] = &[
    ReceiverDescriptor::logitech(0xc52b, ReceiverBrand::Unifying, ReceiverProtocol::Unifying),
    ReceiverDescriptor::logitech(0xc532, ReceiverBrand::Unifying, ReceiverProtocol::Unifying),
    // Nano receiver bundled with the G602.
    ReceiverDescriptor::logitech(0xc537, ReceiverBrand::Nano, ReceiverProtocol::Unifying),
    // Lightspeed gaming receiver used by the G502, G Pro Wireless, G604, and
    // other G-series devices.
    ReceiverDescriptor::logitech(
        0xc539,
        ReceiverBrand::Lightspeed,
        ReceiverProtocol::Unifying,
    ),
    // Lightspeed nano receiver, verified with a G305 (WPID 0x4074).
    ReceiverDescriptor::logitech(
        0xc53f,
        ReceiverBrand::Lightspeed,
        ReceiverProtocol::Unifying,
    ),
    // Lightspeed receiver, verified with a G915 (WPID 0x407c) and G502 X.
    ReceiverDescriptor::logitech(
        0xc547,
        ReceiverBrand::Lightspeed,
        ReceiverProtocol::Unifying,
    ),
    ReceiverDescriptor::logitech(0xc548, ReceiverBrand::Bolt, ReceiverProtocol::Bolt),
    // Lightspeed receiver, verified with a PRO X SUPERLIGHT 2 DEX (WPID 0x40b8).
    ReceiverDescriptor::logitech(
        0xc54d,
        ReceiverBrand::Lightspeed,
        ReceiverProtocol::Unifying,
    ),
];

/// Finds the descriptor for an exact USB vendor/product identity.
#[must_use]
pub fn find_receiver(vendor_id: u16, product_id: u16) -> Option<&'static ReceiverDescriptor> {
    RECEIVERS
        .iter()
        .find(|receiver| receiver.vendor_id == vendor_id && receiver.product_id == product_id)
}

#[cfg(test)]
mod tests {
    use super::{RECEIVERS, ReceiverBrand, ReceiverProtocol, find_receiver};
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn receiver_identities_are_unique() {
        for (index, receiver) in RECEIVERS.iter().enumerate() {
            assert!(
                RECEIVERS[..index].iter().all(|other| {
                    (other.vendor_id, other.product_id) != (receiver.vendor_id, receiver.product_id)
                }),
                "duplicate receiver identity {:04x}:{:04x}",
                receiver.vendor_id,
                receiver.product_id
            );
        }
    }

    #[test]
    fn superlight_dex_receiver_is_lightspeed_over_unifying_protocol() {
        let receiver = find_receiver(LOGITECH_VENDOR_ID, 0xc54d).expect("c54d receiver");

        assert_eq!(receiver.brand, ReceiverBrand::Lightspeed);
        assert_eq!(receiver.protocol, ReceiverProtocol::Unifying);
    }

    #[test]
    fn lookup_requires_the_matching_vendor() {
        assert!(find_receiver(0xffff, 0xc54d).is_none());
    }
}
