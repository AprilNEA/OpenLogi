//! Physical target projection for semantic profile capture.

use anyhow::Result;
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::hid::DeviceRoute;

use super::super::target_selection::{self, FixtureTarget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetLocation {
    Inventory { inventory: usize, device: usize },
    Standalone { device: usize },
}

#[derive(Clone, Debug)]
pub(super) struct TargetCandidate {
    route: DeviceRoute,
    name: String,
    pub(super) location: TargetLocation,
}

impl FixtureTarget for TargetCandidate {
    fn route(&self) -> &DeviceRoute {
        &self.route
    }

    fn display_name(&self) -> &str {
        &self.name
    }
}

pub(super) fn target_candidates(
    inventories: &[DeviceInventory],
    standalone: &[StandaloneDevice],
) -> Vec<TargetCandidate> {
    let hidpp = inventories
        .iter()
        .enumerate()
        .flat_map(|(inventory_index, inventory)| {
            inventory
                .paired
                .iter()
                .enumerate()
                .filter_map(move |(device_index, device)| {
                    let route = DeviceRoute::device_route_for(inventory, device.slot)?;
                    Some(TargetCandidate {
                        route,
                        name: device
                            .codename
                            .clone()
                            .unwrap_or_else(|| format!("Slot {}", device.slot)),
                        location: TargetLocation::Inventory {
                            inventory: inventory_index,
                            device: device_index,
                        },
                    })
                })
        });
    let standalone = standalone
        .iter()
        .enumerate()
        .map(|(device, standalone)| TargetCandidate {
            route: standalone_route(standalone),
            name: standalone.display_name.clone(),
            location: TargetLocation::Standalone { device },
        });
    hidpp.chain(standalone).collect()
}

pub(super) fn select_target(
    candidates: &[TargetCandidate],
    query: Option<&str>,
) -> Result<TargetCandidate> {
    target_selection::select_target(candidates, query)
}

pub(super) fn standalone_route(device: &StandaloneDevice) -> DeviceRoute {
    DeviceRoute::RawHid {
        vendor_id: device.address.vendor_id,
        product_id: device.address.product_id,
        usage_page: device.address.usage_page,
        usage_id: device.address.usage_id,
        identity: device.address.identity.clone(),
    }
}
