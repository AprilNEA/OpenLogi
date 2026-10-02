//! The agent's device list: built from an inventory snapshot and ordered the
//! way the GUI carousel is, then diffed across ticks into the volatile-settings
//! re-apply plan and the host-switch links.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use openlogi_core::config::Config;
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};

use super::AgentDevice;
use crate::hardware::WheelModeChange;
use crate::watchers::host_switch::HostSwitchLink;

/// Resolve the two independently-gated HiResWheel settings for one device
/// into the change that applies them. A setting the device cannot take, or
/// that is not configured, is left out of the change and keeps the device's
/// current value; `None` when that leaves nothing to write.
pub(super) fn configured_wheel_mode(config: &Config, dev: &AgentDevice) -> Option<WheelModeChange> {
    let capabilities = dev.capabilities?;
    let route_key = stable_id(dev).route_key();
    let device = config.devices.get(dev.config_key.as_str());
    let resolution = capabilities
        .hires_wheel
        .then(|| device.and_then(|d| d.effective_scroll_resolution(&route_key)))
        .flatten();
    let inverted = capabilities
        .scroll_inversion
        .then(|| device.is_some_and(|d| d.effective_invert_scroll(&route_key)));
    WheelModeChange::new(resolution, inverted)
}

/// Build the agent device list from an inventory snapshot. Mirrors the GUI's
/// `build_device_list` minus the asset/display fields: a device is included
/// only once its HID++ DeviceInformation (`model_info`) has resolved, since the
/// model key is derived from it.
///
/// `config` is read, never written: [`Config::resolve_device_key`] needs it to
/// answer where a device's settings actually live, which depends on what the
/// persisted `links` index and the existing entries say. The agent never
/// adopts a route — that is the GUI's job — so this call cannot change the
/// answer for the next tick.
pub(super) fn build_devices(
    config: &Config,
    inventories: &[DeviceInventory],
    standalone: &[StandaloneDevice],
) -> Vec<AgentDevice> {
    let mut devices = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            let route = DeviceRoute::for_slot(inv, paired.slot);
            let stable_id = DeviceStableId::from_parts(
                route.as_ref(),
                paired.slot,
                model.serial_number.as_deref(),
                model.unit_id,
            );
            // Always offer the probe's identity; `resolve_hint` / `is_physical`
            // drop empty serial / all-zero unit so those stay route-keyed.
            // Offline Easy-Switch siblings often retain a cached serial (#1560).
            let identity =
                DeviceIdentity::from_parts(model.serial_number.as_deref(), model.unit_id);
            let Some(config_key) = config.resolve_device_key(&stable_id, identity.resolve_hint())
            else {
                continue;
            };
            devices.push(AgentDevice {
                config_key: config_key.into_string(),
                model_key: model.model_key(),
                route,
                slot: paired.slot,
                serial: model.serial_number.clone(),
                unit_id: model.unit_id,
                capabilities: paired.capabilities,
                kind: paired.kind,
                light_capabilities: None,
                online: paired.online,
            });
        }
    }
    for device in standalone {
        let route = device.route();
        let stable_id = DeviceStableId::from_parts(
            Some(&route),
            DIRECT_DEVICE_INDEX,
            device.serial_number.as_deref(),
            device.unit_id,
        );
        let identity = DeviceIdentity::from_parts(device.serial_number.as_deref(), device.unit_id);
        let Some(config_key) = config.resolve_device_key(&stable_id, identity.resolve_hint())
        else {
            continue;
        };
        devices.push(AgentDevice {
            config_key: config_key.into_string(),
            model_key: device.display_name.clone(),
            route: Some(route),
            slot: DIRECT_DEVICE_INDEX,
            serial: device.serial_number.clone(),
            unit_id: device.unit_id,
            capabilities: device.capabilities,
            kind: device.kind,
            light_capabilities: device.light_capabilities,
            online: device.online,
        });
    }
    let mut devices = fold_devices_by_config_key(config, devices);
    // Order by the same canonical key the GUI carousel uses, so the
    // no-saved-selection fallback (`pick_current` -> index 0) targets the device
    // the GUI shows first rather than whatever HID node enumerated first.
    // `config_key` only breaks ties a unique `DeviceStableId` never produces.
    devices.sort_by(|a, b| {
        stable_id(a)
            .cmp(&stable_id(b))
            .then_with(|| a.model_key.cmp(&b.model_key))
    });
    devices
}

/// Collapse Easy-Switch sibling slots that share one [`AgentDevice::config_key`]
/// into a single agent device — the same fold the GUI applies via
/// `fold_by_inventory_key` (#1560).
///
/// Online always wins. When every candidate is offline, prefer a route already
/// present in that device's `config.links`, else the lowest slot number.
fn fold_devices_by_config_key(config: &Config, devices: Vec<AgentDevice>) -> Vec<AgentDevice> {
    let mut by_key: HashMap<String, AgentDevice> = HashMap::new();
    for device in devices {
        match by_key.entry(device.config_key.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(device);
            }
            Entry::Occupied(mut slot) => {
                if prefer_folded_device(config, &device, slot.get()) {
                    slot.insert(device);
                }
            }
        }
    }
    by_key.into_values().collect()
}

/// Whether `candidate` should replace `incumbent` when both share a config key.
fn prefer_folded_device(config: &Config, candidate: &AgentDevice, incumbent: &AgentDevice) -> bool {
    match (candidate.online, incumbent.online) {
        (false, true) => false,
        // Online candidate wins; both online keeps insertion order ("later
        // wins"), matching the GUI.
        (true, _) => true,
        (false, false) => prefer_offline_survivor(config, candidate, incumbent),
    }
}

fn prefer_offline_survivor(
    config: &Config,
    candidate: &AgentDevice,
    incumbent: &AgentDevice,
) -> bool {
    let candidate_linked = route_in_device_links(config, candidate);
    let incumbent_linked = route_in_device_links(config, incumbent);
    match (candidate_linked, incumbent_linked) {
        (true, false) => true,
        (false, true) => false,
        _ => candidate.slot < incumbent.slot,
    }
}

fn route_in_device_links(config: &Config, device: &AgentDevice) -> bool {
    let Some(entry) = config.devices.get(device.config_key.as_str()) else {
        return false;
    };
    entry
        .links
        .contains_key(stable_id(device).route_key().as_str())
}

pub(super) fn host_switch_links(config: &Config, devices: &[AgentDevice]) -> Vec<HostSwitchLink> {
    config
        .devices
        .iter()
        .filter_map(|(keyboard_key, settings)| {
            let keyboard = devices
                .iter()
                .find(|device| device.config_key == *keyboard_key && device.online)?
                .route
                .clone()?;
            let targets = settings
                .host_switch_targets
                .iter()
                .filter_map(|target_key| {
                    devices
                        .iter()
                        .find(|device| device.config_key == *target_key)
                        .and_then(|device| device.route.clone())
                })
                .collect::<Vec<_>>();
            (!targets.is_empty()).then_some(HostSwitchLink { keyboard, targets })
        })
        .collect()
}

/// The canonical identity of one device: what the GUI carousel orders by, what
/// the config key is derived from, and what [`reapply_targets`] matches a device
/// against across inventory ticks.
pub(super) fn stable_id(dev: &AgentDevice) -> DeviceStableId {
    DeviceStableId::from_parts(
        dev.route.as_ref(),
        dev.slot,
        dev.serial.as_deref(),
        dev.unit_id,
    )
}

/// Indices into `next` of devices whose volatile settings need re-applying:
/// a device whose stable identity is newly present (a first sighting, or a
/// replug that re-enumerated under a new identity — e.g. a Bolt device that
/// moved slots), or an offline→online transition (a reconnect after device
/// sleep); plus — after a system wake — every online device. Devices are
/// matched across ticks by [`stable_id`]. Offline devices are never targeted
/// (the write would just time out); they re-apply on their own transition.
pub(super) fn reapply_targets(
    prev: &[AgentDevice],
    next: &[AgentDevice],
    reapply_all: bool,
) -> Vec<usize> {
    next.iter()
        .enumerate()
        .filter(|(_, dev)| dev.online && dev.route.is_some())
        .filter(|(_, dev)| {
            if reapply_all {
                return true;
            }
            let id = stable_id(dev);
            match prev.iter().find(|p| stable_id(p) == id) {
                // A new identity (first sighting, or a replug under a new
                // route/slot) needs a fresh apply; a known one only when it has
                // just come back online.
                None => true,
                Some(p) => !p.online,
            }
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Whether this refresh invalidated any online device's volatile control
/// diversion. Receiver routes stay connected while a paired mouse sleeps, so
/// route equality alone cannot tell capture sessions to re-arm on wake.
pub(super) fn any_device_needs_capture_rearm(
    prev: &[AgentDevice],
    next: &[AgentDevice],
    reapply_all: bool,
) -> bool {
    !reapply_targets(prev, next, reapply_all).is_empty()
}

/// How many explicit confirmation passes a first-sighted or wake-targeted
/// device keeps re-applying its volatile settings after the initial write. A
/// cold restart leaves a Bolt/Unifying mouse slow to enumerate — and a system
/// wake can enumerate a receiver whose mouse link is still re-establishing —
/// so the first write (and a single confirm) can both time out against a
/// still-booting device. Four confirmations are requested at two-second
/// intervals; any intervening authoritative reconciliation satisfies one.
pub(super) const VOLATILE_REAPPLY_CONFIRM_RETRIES: u8 = 4;

/// Plan this refresh's volatile-settings writes: the [`reapply_targets`] set
/// plus a bounded run of confirming re-applies for devices first sighted
/// recently or targeted by a system wake, and the follow-up keys (with
/// remaining retry counts) to confirm next refresh. Reconnects
/// (offline→online) re-apply once — the device was already booted, so it
/// needs no boot-race retry.
pub(super) fn plan_reapply(
    prev: &[AgentDevice],
    next: &[AgentDevice],
    followup: &HashMap<String, u8>,
    reapply_all: bool,
) -> (Vec<usize>, HashMap<String, u8>) {
    let mut targets = reapply_targets(prev, next, reapply_all);
    let mut next_followup: HashMap<String, u8> = targets
        .iter()
        .filter(|&&idx| {
            reapply_all || {
                let id = stable_id(&next[idx]);
                !prev.iter().any(|p| stable_id(p) == id)
            }
        })
        .map(|&idx| {
            (
                next[idx].config_key.clone(),
                VOLATILE_REAPPLY_CONFIRM_RETRIES,
            )
        })
        .collect();
    for (idx, dev) in next.iter().enumerate() {
        if dev.online
            && dev.route.is_some()
            && !targets.contains(&idx)
            && let Some(&remaining) = followup.get(&dev.config_key)
        {
            targets.push(idx);
            if remaining > 1 {
                next_followup.insert(dev.config_key.clone(), remaining - 1);
            }
        }
    }
    (targets, next_followup)
}

/// Index of the selected HID++ input device. Prefer the saved selection while
/// it is an online input route, otherwise the first online input route. If
/// every input device is offline, preserve the saved selection (or the first
/// input route) so its configuration remains stable. Standalone raw-HID
/// devices participate in inventory and settings re-apply but must never
/// replace the mouse/keyboard capture target when selected in the GUI.
pub(super) fn pick_current(devices: &[AgentDevice], saved: Option<&str>) -> usize {
    let saved = saved.and_then(|key| {
        devices
            .iter()
            .position(|device| device.config_key == key && is_hidpp_device(device))
    });
    saved
        .filter(|&idx| devices[idx].online)
        .or_else(|| {
            devices
                .iter()
                .position(|device| device.online && is_hidpp_device(device))
        })
        .or(saved)
        .or_else(|| devices.iter().position(is_hidpp_device))
        .unwrap_or(0)
}

pub(super) fn is_hidpp_device(device: &AgentDevice) -> bool {
    !matches!(device.route, Some(DeviceRoute::RawHid { .. }))
}
