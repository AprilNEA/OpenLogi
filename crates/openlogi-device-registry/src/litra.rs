//! Logitech Litra raw-HID identities.

use crate::LOGITECH_VENDOR_ID;

/// Stable driver-family identifier carried by standalone Litra inventory.
pub const LITRA_DRIVER_ID: &str = "litra";
/// Litra Glow product ID (USB).
pub const LITRA_GLOW_PRODUCT_ID: u16 = 0xc900;
/// Litra Beam product ID (USB).
pub const LITRA_BEAM_PRODUCT_ID: u16 = 0xc901;
/// Litra Glow product ID (BLT).
pub const LITRA_GLOW_BLUETOOTH_PRODUCT_ID: u16 = 0xb900;
/// Litra Beam product ID (BLT).
pub const LITRA_BEAM_BLUETOOTH_PRODUCT_ID: u16 = 0xb901;
/// Litra vendor usage page.
pub const LITRA_USAGE_PAGE: u16 = 0xff43;
/// Litra vendor usage ID.
pub const LITRA_USAGE_ID: u16 = 0x0202;

/// A supported Litra product model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LitraModel {
    /// Logitech Litra Glow.
    Glow,
    /// Logitech Litra Beam.
    Beam,
}

/// A known Litra raw-HID identity and its static product metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LitraDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID.
    pub product_id: u16,
    /// HID usage page of the writable vendor collection.
    pub usage_page: u16,
    /// HID usage ID of the writable vendor collection.
    pub usage_id: u16,
    /// Product model selected by this identity.
    pub model: LitraModel,
    /// Stable standalone-driver family identifier.
    pub driver_id: &'static str,
    /// Exact model identifier used by the OpenLogi asset registry.
    pub registry_model_id: &'static str,
}

impl LitraDescriptor {
    const fn logitech(product_id: u16, model: LitraModel, registry_model_id: &'static str) -> Self {
        Self {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id,
            usage_page: LITRA_USAGE_PAGE,
            usage_id: LITRA_USAGE_ID,
            model,
            driver_id: LITRA_DRIVER_ID,
            registry_model_id,
        }
    }
}

/// All standalone Litra raw-HID identities supported by OpenLogi.
///
/// The Bluetooth entries share their USB counterpart's `registry_model_id`:
/// it is the same physical product and asset artwork, addressed under a
/// different product ID only because that is how it enumerates over that
/// transport.
pub const LITRA_DEVICES: &[LitraDescriptor] = &[
    LitraDescriptor::logitech(LITRA_GLOW_PRODUCT_ID, LitraModel::Glow, "8c900"),
    LitraDescriptor::logitech(LITRA_BEAM_PRODUCT_ID, LitraModel::Beam, "8c901"),
    LitraDescriptor::logitech(LITRA_GLOW_BLUETOOTH_PRODUCT_ID, LitraModel::Glow, "8c900"),
    LitraDescriptor::logitech(LITRA_BEAM_BLUETOOTH_PRODUCT_ID, LitraModel::Beam, "8c901"),
];

/// Finds a Litra descriptor by its complete writable raw-HID identity.
#[must_use]
pub fn find_litra(
    vendor_id: u16,
    product_id: u16,
    usage_page: u16,
    usage_id: u16,
) -> Option<&'static LitraDescriptor> {
    LITRA_DEVICES.iter().find(|device| {
        device.vendor_id == vendor_id
            && device.product_id == product_id
            && device.usage_page == usage_page
            && device.usage_id == usage_id
    })
}

/// Whether an HID descriptor identifies a supported Litra interface.
#[must_use]
pub fn matches_litra(vendor_id: u16, product_id: u16, usage_page: u16, usage_id: u16) -> bool {
    find_litra(vendor_id, product_id, usage_page, usage_id).is_some()
}

/// Whether `product_id` is a Litra light's Bluetooth identity rather than its
/// USB one. Used to pick a connection glyph: a raw-HID Litra route carries no
/// other signal of which transport it is on, unlike an HID++ device's
/// measured transport table.
#[must_use]
pub fn is_litra_bluetooth_product_id(product_id: u16) -> bool {
    matches!(
        product_id,
        LITRA_GLOW_BLUETOOTH_PRODUCT_ID | LITRA_BEAM_BLUETOOTH_PRODUCT_ID
    )
}

#[cfg(test)]
mod tests {
    use super::{LITRA_DEVICES, LITRA_DRIVER_ID, LitraModel, find_litra};
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn litra_identities_are_unique() {
        for (index, device) in LITRA_DEVICES.iter().enumerate() {
            assert!(
                LITRA_DEVICES[..index].iter().all(|other| {
                    (
                        other.vendor_id,
                        other.product_id,
                        other.usage_page,
                        other.usage_id,
                    ) != (
                        device.vendor_id,
                        device.product_id,
                        device.usage_page,
                        device.usage_id,
                    )
                }),
                "duplicate Litra identity {:04x}:{:04x}/{:04x}:{:04x}",
                device.vendor_id,
                device.product_id,
                device.usage_page,
                device.usage_id
            );
        }
    }

    #[test]
    fn complete_identity_selects_glow_metadata() {
        let glow =
            find_litra(LOGITECH_VENDOR_ID, 0xc900, 0xff43, 0x0202).expect("Litra Glow identity");

        assert_eq!(glow.model, LitraModel::Glow);
        assert_eq!(glow.driver_id, LITRA_DRIVER_ID);
        assert_eq!(glow.registry_model_id, "8c900");
        assert!(find_litra(LOGITECH_VENDOR_ID, 0xc900, 0xff43, 0x0203).is_none());
    }

    #[test]
    fn complete_identity_selects_beam_metadata() {
        let beam =
            find_litra(LOGITECH_VENDOR_ID, 0xc901, 0xff43, 0x0202).expect("Litra Beam identity");

        assert_eq!(beam.model, LitraModel::Beam);
        assert_eq!(beam.registry_model_id, "8c901");
    }

    #[test]
    fn a_bluetooth_beam_resolves_to_the_same_model_and_asset_as_its_usb_counterpart() {
        let usb = find_litra(LOGITECH_VENDOR_ID, 0xc901, 0xff43, 0x0202).expect("USB Litra Beam");
        let bluetooth =
            find_litra(LOGITECH_VENDOR_ID, 0xb901, 0xff43, 0x0202).expect("Bluetooth Litra Beam");

        assert_eq!(bluetooth.model, LitraModel::Beam);
        assert_eq!(
            bluetooth.registry_model_id, usb.registry_model_id,
            "same physical product, same asset artwork, regardless of transport"
        );
    }

    #[test]
    fn only_the_bluetooth_product_ids_report_bluetooth() {
        use super::is_litra_bluetooth_product_id;

        assert!(is_litra_bluetooth_product_id(0xb900));
        assert!(is_litra_bluetooth_product_id(0xb901));
        assert!(!is_litra_bluetooth_product_id(0xc900));
        assert!(!is_litra_bluetooth_product_id(0xc901));
    }
}
