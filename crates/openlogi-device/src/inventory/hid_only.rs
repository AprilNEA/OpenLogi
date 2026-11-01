//! Logitech products the host lists while it offers no HID++ node.
//!
//! An empty HID++ inventory has two different causes: no Logitech hardware at
//! all, or hardware whose HID++ interface the host does not expose. A wired
//! G502 HERO on Windows whose vendor-defined collections were disabled
//! (#2188) still moves the cursor through its mouse collection, so the host
//! lists the product while `enumerate_hidpp` returns nothing. Naming that
//! product is what tells the user the problem is on the host side.

use std::collections::HashSet;

use super::InventoryError;
use super::standalone::standalone_device;
use crate::LOGITECH_VENDOR_ID;
use crate::backend::{HidBackend, NodeInfo};

/// A Logitech HID product the host lists while it offers no HID++ node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidOnlyProduct {
    /// HID product id. The vendor is always Logitech.
    pub product_id: u16,
    /// Name the host reports for the product's first listed node.
    pub name: String,
}

/// Enumerate the Logitech products the host lists while it exposes no HID++
/// node at all.
///
/// Products a standalone driver claims (Litra, G600, gaming headsets) are not
/// listed: they need no HID++ node.
///
/// A standalone driver claims a product through one of its collections. When
/// that collection is missing too (a G600 whose vendor-defined collection is
/// disabled or failed to start), the product is listed: its plain collections
/// are all the host offers, and the fix is the same as for a HID++ product.
pub async fn enumerate_hid_only(
    backend: &dyn HidBackend,
) -> Result<Vec<HidOnlyProduct>, InventoryError> {
    let all = backend.enumerate().await?;
    let hidpp = backend.enumerate_hidpp().await?;
    Ok(hid_only_products(&all, &hidpp))
}

/// The Logitech products in `all`, one entry per product id in the order the
/// host lists them, when `hidpp` is empty.
///
/// The verdict is host-wide, not per product. On Linux, `hid-logitech-dj`
/// publishes a child node for each paired device. That node carries the
/// receiver's HID++ collection but is reached through the receiver, so
/// `enumerate_hidpp` leaves it out. A per-product comparison would name a
/// healthy paired mouse as HID-only while its receiver is listed.
fn hid_only_products(all: &[NodeInfo], hidpp: &[NodeInfo]) -> Vec<HidOnlyProduct> {
    if !hidpp.is_empty() {
        return Vec::new();
    }
    let logitech = || {
        all.iter()
            .filter(|node| node.vendor_id == LOGITECH_VENDOR_ID)
    };
    let claimed: HashSet<u16> = logitech()
        .filter(|node| standalone_device(node).is_some())
        .map(|node| node.product_id)
        .collect();
    let mut seen = HashSet::new();
    logitech()
        .filter(|node| !claimed.contains(&node.product_id))
        .filter(|node| seen.insert(node.product_id))
        .map(|node| HidOnlyProduct {
            product_id: node.product_id,
            name: node.name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{HidOnlyProduct, hid_only_products};
    use crate::backend::{NodeId, NodeInfo};

    fn node(vendor_id: u16, product_id: u16, usage: (u16, u16), name: &str) -> NodeInfo {
        NodeInfo {
            id: NodeId::from(format!("{vendor_id:04x}:{product_id:04x}:{:04x}", usage.0)),
            vendor_id,
            product_id,
            usage_page: usage.0,
            usage_id: usage.1,
            name: name.into(),
            manufacturer: Some("Logitech".into()),
            serial_number: None,
        }
    }

    /// The nodes the Windows PC in #2188 listed for its G502 HERO: the mouse,
    /// keyboard, consumer and system collections, with both vendor-defined
    /// collections disabled (`CM_PROB_DISABLED`) and therefore absent.
    fn g502_without_vendor_collections() -> Vec<NodeInfo> {
        [
            (0x000c, 0x0001),
            (0x0001, 0x0080),
            (0x0001, 0x0006),
            (0x0001, 0x0002),
        ]
        .into_iter()
        .map(|usage| node(0x046d, 0xc08b, usage, "G502 HERO Gaming Mouse"))
        .collect()
    }

    #[test]
    fn a_product_with_no_hidpp_node_is_listed_once() {
        let all = g502_without_vendor_collections();
        assert_eq!(
            hid_only_products(&all, &[]),
            vec![HidOnlyProduct {
                product_id: 0xc08b,
                name: "G502 HERO Gaming Mouse".into(),
            }]
        );
    }

    #[test]
    fn any_hidpp_node_on_the_host_lists_nothing() {
        let receiver = node(0x046d, 0xc52b, (0xff00, 0x0002), "USB Receiver");
        let paired_child = node(0x046d, 0x4082, (0x0001, 0x0002), "MX Master 3S");
        let all = vec![receiver.clone(), paired_child];
        assert_eq!(hid_only_products(&all, &[receiver]), Vec::new());
    }

    #[test]
    fn other_vendors_are_ignored() {
        let mut all = g502_without_vendor_collections();
        all.insert(0, node(0x3434, 0x02a1, (0x0001, 0x0002), "Keychron mouse"));
        let products = hid_only_products(&all, &[]);
        assert_eq!(products.len(), 1);
        assert_eq!(products[0].product_id, 0xc08b);
    }

    #[test]
    fn a_product_a_standalone_driver_claims_is_not_listed() {
        let all = vec![
            node(
                0x046d,
                0xc24a,
                (0x0001, 0x0002),
                "Logitech Gaming Mouse G600",
            ),
            node(
                0x046d,
                0xc24a,
                (0xff80, 0x0080),
                "Logitech Gaming Mouse G600",
            ),
        ];
        assert_eq!(hid_only_products(&all, &[]), Vec::new());
    }

    #[test]
    fn a_standalone_product_without_its_claimed_collection_is_listed() {
        let all = vec![
            node(
                0x046d,
                0xc24a,
                (0x0001, 0x0002),
                "Logitech Gaming Mouse G600",
            ),
            node(
                0x046d,
                0xc24a,
                (0x0001, 0x0006),
                "Logitech Gaming Mouse G600",
            ),
        ];
        let ids: Vec<u16> = hid_only_products(&all, &[])
            .into_iter()
            .map(|product| product.product_id)
            .collect();
        assert_eq!(ids, vec![0xc24a]);
    }

    #[test]
    fn products_keep_the_order_the_host_lists_them() {
        let mut all = vec![node(0x046d, 0xc24d, (0x0001, 0x0006), "Gaming Keyboard")];
        all.extend(g502_without_vendor_collections());
        all.push(node(0x046d, 0xc24d, (0x000c, 0x0001), "Gaming Keyboard"));
        let ids: Vec<u16> = hid_only_products(&all, &[])
            .into_iter()
            .map(|product| product.product_id)
            .collect();
        assert_eq!(ids, vec![0xc24d, 0xc08b]);
    }
}
