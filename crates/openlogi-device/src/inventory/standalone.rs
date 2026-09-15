//! Discovery of standalone raw-HID devices.

use std::collections::{HashMap, HashSet};

use openlogi_core::device::{Capabilities, DeviceKind, RawDeviceAddress, StandaloneDevice};
use openlogi_core::hid::LOGITECH_VENDOR_ID;
use openlogi_device_registry::litra::find_litra;

use super::InventoryError;
use crate::backend::{HidBackend, NodeInfo};
use crate::write::litra_capabilities;

/// Enumerate recognized standalone devices without probing them as HID++.
///
/// The returned descriptors are intentionally separate from receiver
/// inventories. A raw device has no HID++ pairing slot and must be routed by
/// its full HID identity tuple.
///
/// One [`HidBackend::enumerate`] call: every node it returns already carries
/// [`NodeInfo::is_hidpp_candidate`], computed by the backend at the same time
/// as everything else here — no second live query is needed (and a second
/// query's own transient failure must never discard what this one already
/// found; see `raw_mice_from_nodes`'s doc for why that matters).
pub async fn enumerate_standalone(
    backend: &dyn HidBackend,
) -> Result<Vec<StandaloneDevice>, InventoryError> {
    let all = backend.enumerate().await?;

    let mut devices: Vec<StandaloneDevice> = all
        .iter()
        .filter_map(|device| {
            let descriptor = find_litra(
                device.vendor_id,
                device.product_id,
                device.usage_page,
                device.usage_id,
            )?;
            let identity = device.identity();
            Some(StandaloneDevice {
                address: RawDeviceAddress {
                    vendor_id: device.vendor_id,
                    product_id: device.product_id,
                    usage_page: device.usage_page,
                    usage_id: device.usage_id,
                    identity,
                },
                display_name: device.name.clone(),
                manufacturer: device.manufacturer.clone(),
                serial_number: device.serial_number.clone(),
                unit_id: [0; 4],
                kind: DeviceKind::Light,
                online: true,
                capabilities: None,
                light_capabilities: Some(litra_capabilities(descriptor.model)),
                driver_id: descriptor.driver_id.to_owned(),
                registry_model_id: Some(descriptor.registry_model_id.to_owned()),
            })
        })
        .collect();
    devices.extend(raw_mice_from_nodes(&all));
    validate_no_ambiguous_nodes(&devices)?;
    Ok(devices)
}

/// The standard USB HID boot-mouse collection: Generic Desktop (`0x0001`) /
/// Mouse (`0x0002`). Cheap Logitech receivers (the M171/M185/M190 family's
/// nano receiver, product id `0xc542`) speak only this — no HID++ collection
/// at all — so no DPI, lighting, or HID++ button remap will ever reach them.
/// They are still real OS pointer devices: `openlogi-hook` already captures
/// any Logitech relative-pointer mouse unconditionally, so Middle/Back/Forward
/// remap and the Actions Ring work for them once they have their own device
/// record and config key.
const BOOT_MOUSE_COLLECTION: (u16, u16) = (0x0001, 0x0002);

fn is_boot_mouse_collection(usage_page: u16, usage_id: u16) -> bool {
    (usage_page, usage_id) == BOOT_MOUSE_COLLECTION
}

/// Capabilities for a raw (non-HID++) OS-hook mouse: buttons are remappable
/// through the OS input hook, but every HID++-only control is permanently
/// unavailable — this hardware has no adjustable DPI, no lighting, and no
/// software wheel/haptics feature to query.
const RAW_MOUSE_CAPABILITIES: Capabilities = Capabilities {
    buttons: true,
    pointer: false,
    lighting: false,
    scroll_inversion: false,
    hires_wheel: false,
    thumbwheel: false,
    haptic_feedback: false,
    haptic_panel: false,
};

/// Stable identifier for the driver family of a raw, non-HID++ mouse. There is
/// no protocol driver behind this — it exists only so the device earns its own
/// [`StandaloneDevice::driver_id`], mirroring the Litra convention.
const RAW_MOUSE_DRIVER_ID: &str = "raw-mouse";

/// Synthesize one [`StandaloneDevice`] per boot-mouse *node* belonging to a
/// physical Logitech device that has *no* HID++ collection at all.
///
/// One entry per node, not one per `(vendor_id, product_id)` group: two
/// identical serial-less receivers (completely normal — that's this exact
/// device's retail packaging) share a product id, and collapsing them to a
/// single representative would silently drop one from inventory before
/// [`validate_no_ambiguous_nodes`] ever got a chance to flag the collision.
/// Each node keeps its own [`raw_mouse_identity`]; two identical units
/// produce the same `stable:` identity (since it is derived only from the
/// fixed HID tuple, not the node), and `validate_no_ambiguous_nodes` rejects
/// that exactly the way it already rejects two identical serial-less Litra
/// lights — same validation path, not a special case for mice.
///
/// Whether the *physical device* has HID++ at all is answered per
/// `(vendor_id, product_id)` group, from [`NodeInfo::is_hidpp_candidate`]
/// already computed on every node in `all` — a real HID++ mouse may
/// additionally expose a boot-mouse collection for BIOS/pre-OS
/// compatibility, and it must not be double-listed as a second,
/// capability-crippled "raw mouse" entry.
fn raw_mice_from_nodes(all: &[NodeInfo]) -> Vec<StandaloneDevice> {
    let mut hidpp_by_device: HashMap<(u16, u16), bool> = HashMap::new();
    for node in all
        .iter()
        .filter(|node| node.vendor_id == LOGITECH_VENDOR_ID)
    {
        let has_hidpp = hidpp_by_device
            .entry((node.vendor_id, node.product_id))
            .or_insert(false);
        *has_hidpp |= node.is_hidpp_candidate;
    }

    let mut devices: Vec<_> = all
        .iter()
        .filter(|node| node.vendor_id == LOGITECH_VENDOR_ID)
        .filter(|node| is_boot_mouse_collection(node.usage_page, node.usage_id))
        .filter(|node| !hidpp_by_device[&(node.vendor_id, node.product_id)])
        .map(|node| StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: node.vendor_id,
                product_id: node.product_id,
                usage_page: node.usage_page,
                usage_id: node.usage_id,
                identity: raw_mouse_identity(node),
            },
            display_name: node.name.clone(),
            manufacturer: node.manufacturer.clone(),
            serial_number: node.serial_number.clone(),
            unit_id: [0; 4],
            kind: DeviceKind::Mouse,
            online: true,
            capabilities: Some(RAW_MOUSE_CAPABILITIES),
            light_capabilities: None,
            driver_id: RAW_MOUSE_DRIVER_ID.to_owned(),
            registry_model_id: None,
        })
        .collect();
    // Deterministic order regardless of backend enumeration order.
    devices.sort_by(|a, b| {
        (a.address.product_id, &a.address.identity)
            .cmp(&(b.address.product_id, &b.address.identity))
    });
    devices
}

/// This class of receiver reports no serial. Fall back to a `stable:` identity
/// derived from its fixed HID identity tuple — recognized as physical by
/// [`openlogi_core::device_order::DeviceStableId::physical_key`] — rather than
/// the backend's transient OS-node id, so a binding set on it survives a
/// restart.
fn raw_mouse_identity(node: &NodeInfo) -> String {
    node.serial_number
        .as_deref()
        .filter(|serial| !serial.is_empty())
        .map_or_else(
            || {
                format!(
                    "stable:{:04x}:{:04x}:{:04x}:{:04x}",
                    node.vendor_id, node.product_id, node.usage_page, node.usage_id
                )
            },
            |serial| format!("serial:{}", serial.to_ascii_lowercase()),
        )
}

/// Reject multiple nodes that the route cannot distinguish safely.
///
/// A serial-bearing pair is distinguishable even when two identical lights
/// share the same VID/PID/usage tuple. An OS-node identity (`id:…`) is only a
/// transient re-find hint, so two such nodes with the same tuple are
/// indistinguishable and must not be exposed as independently selectable
/// devices.
fn validate_no_ambiguous_nodes(devices: &[StandaloneDevice]) -> Result<(), InventoryError> {
    let mut groups: HashMap<(u16, u16, u16, u16), Vec<&StandaloneDevice>> = HashMap::new();
    for device in devices {
        let address = &device.address;
        groups
            .entry((
                address.vendor_id,
                address.product_id,
                address.usage_page,
                address.usage_id,
            ))
            .or_default()
            .push(device);
    }
    if groups.values().any(|group| {
        if group.len() < 2 {
            return false;
        }
        let identities: HashSet<&str> = group
            .iter()
            .map(|device| device.address.identity.as_str())
            .collect();
        identities.len() != group.len()
            || group
                .iter()
                .any(|device| !device.address.identity.starts_with("serial:"))
    }) {
        return Err(InventoryError::AmbiguousRawDevice);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceKind, RawDeviceAddress, StandaloneDevice};

    use crate::backend::{NodeId, NodeInfo};
    use crate::write::matches_litra;

    use super::{InventoryError, raw_mice_from_nodes, validate_no_ambiguous_nodes};

    /// A boot-mouse collection node, as reported by a Logitech nano receiver
    /// that speaks no HID++ at all (e.g. the M171/M185/M190 family, `0xc542`).
    fn boot_mouse_node(id: &str, product_id: u16) -> NodeInfo {
        NodeInfo {
            id: NodeId::from(id.to_owned()),
            vendor_id: 0x046d,
            product_id,
            usage_page: 0x0001,
            usage_id: 0x0002,
            name: "Logitech Wireless Receiver Mouse".into(),
            manufacturer: Some("Logitech".into()),
            serial_number: None,
            is_hidpp_candidate: false,
        }
    }

    /// A HID++ long-report collection node, as a real Unifying/Bolt-paired
    /// mouse would also report on the same physical receiver.
    fn hidpp_node(id: &str, product_id: u16) -> NodeInfo {
        NodeInfo {
            id: NodeId::from(id.to_owned()),
            vendor_id: 0x046d,
            product_id,
            usage_page: 0xff00,
            usage_id: 0x0002,
            name: "HID-compliant mouse".into(),
            manufacturer: Some("Logitech".into()),
            serial_number: None,
            is_hidpp_candidate: true,
        }
    }

    #[test]
    fn boot_mouse_only_device_becomes_a_raw_mouse() {
        let nodes = vec![boot_mouse_node("id:1", 0xc542)];
        let devices = raw_mice_from_nodes(&nodes);
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.kind, DeviceKind::Mouse);
        assert_eq!(device.driver_id, "raw-mouse");
        let caps = device
            .capabilities
            .expect("raw mouse must carry capabilities");
        assert!(caps.buttons);
        assert!(!caps.pointer);
        assert!(!caps.lighting);
        assert!(!caps.scroll_inversion);
        assert!(!caps.hires_wheel);
        assert!(!caps.thumbwheel);
        assert!(!caps.haptic_feedback);
        assert!(!caps.haptic_panel);
        assert_eq!(device.address.identity, "stable:046d:c542:0001:0002");
    }

    #[test]
    fn hidpp_mouse_with_a_boot_mouse_collection_is_not_double_listed() {
        // A real HID++ mouse may also expose a boot-mouse collection for
        // BIOS/pre-OS compatibility — the whole physical device already has a
        // HID++ node, so it must not additionally synthesize a raw entry.
        let nodes = vec![boot_mouse_node("id:1", 0xb023), hidpp_node("id:2", 0xb023)];
        let devices = raw_mice_from_nodes(&nodes);
        assert!(
            devices.is_empty(),
            "an already-HID++ device must not also appear as a raw mouse"
        );
    }

    #[test]
    fn two_different_raw_mice_surface_as_two_records() {
        let nodes = vec![
            boot_mouse_node("id:1", 0xc542),
            boot_mouse_node("id:2", 0xc52f),
        ];
        let devices = raw_mice_from_nodes(&nodes);
        let mut pids: Vec<u16> = devices.iter().map(|d| d.address.product_id).collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![0xc52f, 0xc542]);
    }

    #[test]
    fn non_logitech_boot_mouse_is_ignored() {
        let mut node = boot_mouse_node("id:1", 0xc542);
        node.vendor_id = 0x1234;
        let devices = raw_mice_from_nodes(&[node]);
        assert!(devices.is_empty());
    }

    /// Two identical serial-less nano receivers is this exact device's
    /// normal retail packaging (a pair pack), not an edge case. Each keeps
    /// its own node in `raw_mice_from_nodes`'s output (rather than one
    /// group-representative silently dropping the other), so the identical
    /// `stable:` identity they both compute reaches
    /// `validate_no_ambiguous_nodes` and is rejected there — the exact same
    /// path that already rejects two identical serial-less Litra lights.
    #[test]
    fn two_identical_serial_less_raw_mice_are_flagged_ambiguous_not_collapsed() {
        let nodes = vec![
            boot_mouse_node("id:1", 0xc542),
            boot_mouse_node("id:2", 0xc542),
        ];
        let devices = raw_mice_from_nodes(&nodes);
        assert_eq!(
            devices.len(),
            2,
            "both physical nodes must survive into the pre-validation list"
        );
        assert!(
            matches!(
                validate_no_ambiguous_nodes(&devices),
                Err(InventoryError::AmbiguousRawDevice)
            ),
            "two identical serial-less raw mice must be rejected, not silently merged into one"
        );
    }

    #[test]
    fn glow_fixture_matches_standalone_driver() {
        assert!(matches_litra(0x046d, 0xc900, 0xff43, 0x0202));
    }

    fn raw(identity: &str) -> StandaloneDevice {
        StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: 0x046d,
                product_id: 0xc900,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: identity.into(),
            },
            display_name: "Litra Glow".into(),
            manufacturer: Some("Logi".into()),
            serial_number: identity.strip_prefix("serial:").map(str::to_string),
            unit_id: [0; 4],
            kind: DeviceKind::Light,
            online: true,
            capabilities: None,
            light_capabilities: None,
            driver_id: "litra".into(),
            registry_model_id: None,
        }
    }

    #[test]
    fn duplicate_transient_nodes_are_rejected() {
        let devices = vec![raw("id:old"), raw("id:new")];
        assert!(matches!(
            validate_no_ambiguous_nodes(&devices),
            Err(InventoryError::AmbiguousRawDevice)
        ));
    }

    #[test]
    fn distinct_serials_can_share_the_same_hid_tuple() {
        let devices = vec![raw("serial:one"), raw("serial:two")];
        let validation = validate_no_ambiguous_nodes(&devices);
        assert!(
            validation.is_ok(),
            "serial-backed nodes stay distinguishable: {validation:?}"
        );
    }

    /// A backend whose `enumerate_hidpp` is broken (device I/O suspended, a
    /// transient transport error, whatever) must not cost `enumerate_standalone`
    /// anything: the raw-mouse/HID++ dedup answer comes from
    /// `NodeInfo::is_hidpp_candidate`, already carried on every node
    /// `enumerate` returned, so a second, separately-fallible backend query
    /// is never made in the first place.
    struct BrokenHidppQueryBackend {
        nodes: Vec<NodeInfo>,
    }

    #[hidpp::async_trait]
    impl crate::backend::HidBackend for BrokenHidppQueryBackend {
        async fn enumerate(&self) -> Result<Vec<NodeInfo>, crate::backend::BackendError> {
            Ok(self.nodes.clone())
        }

        async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, crate::backend::BackendError> {
            Err(crate::backend::BackendError::Backend(
                "transient transport error".into(),
            ))
        }

        async fn open_hidpp(
            &self,
            _node: &NodeInfo,
        ) -> Result<
            Option<std::sync::Arc<hidpp::channel::HidppChannel>>,
            crate::backend::BackendError,
        > {
            Err(crate::backend::BackendError::Disconnected)
        }

        async fn open_raw_writer(
            &self,
            _node: &NodeInfo,
        ) -> Result<Box<dyn crate::backend::RawWriter>, crate::backend::BackendError> {
            Err(crate::backend::BackendError::Disconnected)
        }

        fn watch(&self) -> Result<crate::backend::HotplugStream, crate::backend::BackendError> {
            Ok(Box::new(futures_lite::stream::empty()))
        }
    }

    #[tokio::test]
    async fn a_broken_second_hidpp_query_does_not_discard_standalone_results() {
        let backend = BrokenHidppQueryBackend {
            nodes: vec![boot_mouse_node("id:1", 0xc542)],
        };
        let devices = super::enumerate_standalone(&backend)
            .await
            .expect("enumerate_standalone must not fail on a call it never makes");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].driver_id, "raw-mouse");
    }
}
