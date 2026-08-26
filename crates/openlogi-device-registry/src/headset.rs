//! Logitech gaming-headset raw-HID identities.
//!
//! These headsets don't speak the eQuad HID++ receiver protocol mice and
//! keyboards use. Their dongle exposes a proprietary vendor collection
//! instead, and no publicly documented reverse-engineering of that protocol
//! is known yet (checked against the `HeadsetControl` project's Logitech
//! drivers, none of which match this family's usage page). So, like Litra,
//! these are surfaced as standalone raw-HID devices for identity only: no
//! battery, mute, or sidetone control until the wire protocol is worked out.

use crate::LOGITECH_VENDOR_ID;

/// Stable driver-family identifier carried by standalone headset inventory.
pub const GAMING_HEADSET_DRIVER_ID: &str = "logitech-gaming-headset";

/// A known Logitech gaming-headset raw-HID identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GamingHeadsetDescriptor {
    /// USB vendor ID.
    pub vendor_id: u16,
    /// USB product ID of the headset's dongle.
    pub product_id: u16,
    /// HID usage page of the vendor collection used as this device's stable
    /// identity node.
    pub usage_page: u16,
    /// HID usage ID of that vendor collection.
    pub usage_id: u16,
    /// Marketed product name.
    pub name: &'static str,
}

impl GamingHeadsetDescriptor {
    const fn logitech(product_id: u16, usage_page: u16, usage_id: u16, name: &'static str) -> Self {
        Self {
            vendor_id: LOGITECH_VENDOR_ID,
            product_id,
            usage_page,
            usage_id,
            name,
        }
    }
}

/// All standalone gaming-headset raw-HID identities OpenLogi recognizes.
pub const GAMING_HEADSETS: &[GamingHeadsetDescriptor] = &[
    // G735 LIGHTSPEED. Its dongle (046D:0AD8) exposes vendor collections on
    // 0xff00/0x0001 and 0xff03/0x0001..0x0003; 0xff00/0x0001 is used here as
    // the stable identity node, matching the precedent of the older G533/G930
    // vendor page. Identity only — no control protocol implemented.
    GamingHeadsetDescriptor::logitech(0x0ad8, 0xff00, 0x0001, "G735 Gaming Headset"),
];

/// Finds a gaming-headset descriptor by its complete raw-HID identity.
#[must_use]
pub fn find_gaming_headset(
    vendor_id: u16,
    product_id: u16,
    usage_page: u16,
    usage_id: u16,
) -> Option<&'static GamingHeadsetDescriptor> {
    GAMING_HEADSETS.iter().find(|device| {
        device.vendor_id == vendor_id
            && device.product_id == product_id
            && device.usage_page == usage_page
            && device.usage_id == usage_id
    })
}

#[cfg(test)]
mod tests {
    use super::{GAMING_HEADSET_DRIVER_ID, GAMING_HEADSETS, find_gaming_headset};
    use crate::LOGITECH_VENDOR_ID;

    #[test]
    fn gaming_headset_identities_are_unique() {
        for (index, device) in GAMING_HEADSETS.iter().enumerate() {
            assert!(
                GAMING_HEADSETS[..index].iter().all(|other| {
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
                "duplicate gaming headset identity {:04x}:{:04x}/{:04x}:{:04x}",
                device.vendor_id,
                device.product_id,
                device.usage_page,
                device.usage_id
            );
        }
    }

    #[test]
    fn complete_identity_selects_g735_metadata() {
        let g735 =
            find_gaming_headset(LOGITECH_VENDOR_ID, 0x0ad8, 0xff00, 0x0001).expect("G735 identity");

        assert_eq!(g735.name, "G735 Gaming Headset");
        assert!(find_gaming_headset(LOGITECH_VENDOR_ID, 0x0ad8, 0xff03, 0x0001).is_none());
    }

    #[test]
    fn lookup_requires_the_matching_vendor() {
        assert!(find_gaming_headset(0xffff, 0x0ad8, 0xff00, 0x0001).is_none());
    }

    #[test]
    fn driver_id_is_stable() {
        assert_eq!(GAMING_HEADSET_DRIVER_ID, "logitech-gaming-headset");
    }
}
