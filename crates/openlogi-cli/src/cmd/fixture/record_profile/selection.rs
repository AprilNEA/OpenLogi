//! Strict physical target selection with privacy-safe diagnostics.

use std::fmt::Write as _;

use anyhow::{Result, anyhow, bail};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::hid::DeviceRoute;

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
    if candidates.is_empty() {
        bail!("the Agent snapshot contains no addressable physical device candidate");
    }

    let mut matches = match query {
        Some(query) => candidates
            .iter()
            .filter(|candidate| {
                candidate.name.eq_ignore_ascii_case(query) || candidate.route.to_string() == query
            })
            .cloned()
            .collect::<Vec<_>>(),
        None if candidates.len() == 1 => candidates.to_owned(),
        None => Vec::new(),
    };
    if matches.len() != 1 {
        let message = match (query, matches.len()) {
            (Some(_), 0) => "no candidate exactly matched --device",
            (Some(_), _) => "--device matched more than one candidate",
            (None, _) => "more than one physical candidate is available; pass --device",
        };
        return Err(selection_error(message, candidates));
    }

    let selected = matches.remove(0);
    if candidates
        .iter()
        .filter(|candidate| candidate.route == selected.route)
        .count()
        != 1
    {
        return Err(selection_error(
            "the selected route is duplicated and cannot identify one target",
            candidates,
        ));
    }
    Ok(selected)
}

fn selection_error(message: &str, candidates: &[TargetCandidate]) -> anyhow::Error {
    let mut list = String::new();
    for candidate in candidates {
        let _ = write!(
            list,
            "\n  - {:?} ({})",
            candidate.name,
            safe_route_label(&candidate.route)
        );
    }
    anyhow!("{message}. Candidates:{list}")
}

fn safe_route_label(route: &DeviceRoute) -> String {
    match route {
        DeviceRoute::Bolt { slot, .. } => format!("Bolt receiver slot {slot}"),
        DeviceRoute::Unifying { slot, .. } => format!("Unifying receiver slot {slot}"),
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => format!("direct {vendor_id:04x}:{product_id:04x}"),
        DeviceRoute::RawHid {
            vendor_id,
            product_id,
            usage_page,
            usage_id,
            ..
        } => format!(
            "standalone {vendor_id:04x}:{product_id:04x} usage {usage_page:04x}:{usage_id:04x}"
        ),
    }
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
