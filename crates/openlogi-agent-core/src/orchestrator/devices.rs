//! The agent's device list: built from an inventory snapshot and ordered the
//! way the GUI carousel is, then diffed across ticks into the volatile-settings
//! re-apply plan and the host-switch links.

use std::collections::HashMap;

use openlogi_core::config::{Config, ScrollResolution};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_hid::session::gesture::MainWheelDivert;
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute, ReportRate};

use super::AgentDevice;
use crate::hardware::WheelModeChange;
use crate::watchers::host_switch::HostSwitchLink;

/// Resolve the two independently-gated HiResWheel settings for one device
/// into the change that applies them. A setting the device cannot take, or
/// that is not configured, is left out of the change and keeps the device's
/// current value; `None` when that leaves nothing to write, and while the
/// device's capture session owns the wheel ([`main_wheel_divert`]): that
/// session writes the same settings along with its divert, and a write from
/// here would hand the wheel back to native reporting under it.
pub(super) fn configured_wheel_mode(
    config: &Config,
    dev: &AgentDevice,
    os_mouse_hook_available: bool,
) -> Option<WheelModeChange> {
    if main_wheel_divert(config, dev, os_mouse_hook_available).is_some() {
        return None;
    }
    let (resolution, inverted) = wheel_settings(config, dev)?;
    WheelModeChange::new(resolution, inverted)
}

/// The main-wheel divert `dev`'s capture session arms for smooth scrolling,
/// carrying the wheel settings [`configured_wheel_mode`] would otherwise
/// write; `None` leaves the wheel native.
///
/// Diverting needs the device's opt-in, its `0x2121` wheel, and an agent that
/// may post the scrolling in the wheel's place: the mouse hook running (on
/// macOS, Accessibility granted) and mouse capture not switched off. Logi
/// Options+ keeps the wheel native without Accessibility the same way.
pub(super) fn main_wheel_divert(
    config: &Config,
    dev: &AgentDevice,
    os_mouse_hook_available: bool,
) -> Option<MainWheelDivert> {
    let wanted = os_mouse_hook_available
        && config.app_settings.capture_mouse_events
        && config.device_enabled(&dev.config_key)
        && config.smooth_wheel(&dev.config_key)
        && dev.capabilities.is_some_and(|caps| caps.hires_wheel);
    if !wanted {
        return None;
    }
    let (resolution, inverted) = wheel_settings(config, dev)?;
    Some(MainWheelDivert {
        resolution,
        inverted,
    })
}

/// The configured wheel resolution and native inversion for `dev`, each
/// `None` where the device cannot take it or nothing is configured; `None`
/// overall while the device's capabilities are unknown.
fn wheel_settings(
    config: &Config,
    dev: &AgentDevice,
) -> Option<(Option<ScrollResolution>, Option<bool>)> {
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
    Some((resolution, inverted))
}

/// Whether the scrolling the agent posts for `dev`'s main wheel is reversed —
/// a wheel-hold button's horizontal scroll and the diverted smooth wheel: the
/// inversion its native wheel gets. A wheel with the native `0x2121`
/// invert bit is written the value effective on the link `dev` is reached
/// by ([`configured_wheel_mode`]); one without it is reversed by the hook
/// from the device-level setting (`Orchestrator::publish_scroll_inversion`).
/// Unknown capabilities write and reverse nothing, so nothing is inverted.
pub(super) fn wheel_inverted(config: &Config, dev: &AgentDevice) -> bool {
    match dev.capabilities {
        Some(capabilities) if capabilities.scroll_inversion => config
            .devices
            .get(dev.config_key.as_str())
            .is_some_and(|d| d.effective_invert_scroll(&stable_id(dev).route_key())),
        Some(_) => config.invert_scroll(&dev.config_key),
        None => false,
    }
}

/// The saved report rate to push to `dev`: only to a device with a
/// report-rate feature, so a rate saved for a mouse is never written to
/// whatever later answers under the same key without one.
pub(super) fn configured_report_rate(config: &Config, dev: &AgentDevice) -> Option<ReportRate> {
    dev.capabilities
        .is_some_and(|caps| caps.report_rate)
        .then(|| config.report_rate(&dev.config_key))
        .flatten()
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
            // An offline probe reports an all-zero unit id, which is not a
            // physical identity — offer it only while the device is online,
            // exactly as the GUI does, or every sleeping device would resolve
            // to the same non-key.
            let identity =
                DeviceIdentity::from_parts(model.serial_number.as_deref(), model.unit_id);
            let Some(config_key) =
                config.resolve_device_key(&stable_id, paired.online.then_some(&identity))
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
                settings_revision: paired.settings_revision,
                codename: paired.codename.clone(),
                battery: paired.battery.clone(),
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
        let Some(config_key) =
            config.resolve_device_key(&stable_id, device.online.then_some(&identity))
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
            // A standalone device reports no device-side setting changes.
            settings_revision: 0,
            // Its name is its display name, and it reports no battery.
            codename: None,
            battery: None,
        });
    }
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

/// Indices into `next` of online devices whose settings revision moved since
/// `prev`: the device reported a setting changed on it. A device is matched by
/// [`stable_id`] and must keep its route, as the revision restarts when the
/// device's channel is reopened.
pub(super) fn settings_moved(prev: &[AgentDevice], next: &[AgentDevice]) -> Vec<usize> {
    next.iter()
        .enumerate()
        .filter(|(_, dev)| dev.online && dev.route.is_some())
        .filter(|(_, dev)| {
            let id = stable_id(dev);
            prev.iter().find(|p| stable_id(p) == id).is_some_and(|p| {
                p.online && p.route == dev.route && p.settings_revision != dev.settings_revision
            })
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Indices into `next` of online devices that report the `os_layout`
/// capability on a route that was already online without it, such as a Bolt
/// keyboard whose feature walk failed until after its reconnect reapply. A
/// device that just came online or changed route is reapplied instead.
pub(super) fn os_layout_appeared(prev: &[AgentDevice], next: &[AgentDevice]) -> Vec<usize> {
    let has_os_layout = |dev: &AgentDevice| dev.capabilities.is_some_and(|caps| caps.os_layout);
    next.iter()
        .enumerate()
        .filter(|(_, dev)| dev.online && dev.route.is_some() && has_os_layout(dev))
        .filter(|(_, dev)| {
            let id = stable_id(dev);
            prev.iter()
                .find(|p| stable_id(p) == id)
                .is_some_and(|p| p.online && p.route == dev.route && !has_os_layout(p))
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// Whether `config` keeps the key layout of the device under `key` on the
/// host OS, whatever the device supports: enabled, with the setting on.
pub(super) fn config_keeps_os_layout(config: &Config, key: &str) -> bool {
    config.device_enabled(key) && config.keep_os_layout(key)
}

/// Whether `config` keeps `dev`'s key layout on the host OS: a keyboard whose
/// platform software can set, enabled, with the setting on.
pub(super) fn keeps_os_layout(config: &Config, dev: &AgentDevice) -> bool {
    dev.capabilities.is_some_and(|caps| caps.os_layout)
        && config_keeps_os_layout(config, &dev.config_key)
}

/// The devices whose "keep OS layout" takes effect going from `previous` to
/// `config`: the setting turned on, or its device re-enabled.
pub(super) fn newly_kept_os_layouts<'a>(
    previous: &'a Config,
    config: &'a Config,
    devices: &'a [AgentDevice],
) -> impl Iterator<Item = &'a AgentDevice> {
    devices
        .iter()
        .filter(|dev| keeps_os_layout(config, dev) && !keeps_os_layout(previous, dev))
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

/// The shorter confirmation budget for a link re-establishment (a device nap
/// ending in an offline→online transition or a `0x1d4b` reconnect broadcast).
/// The device was already booted, so it needs no boot-race ladder — but its
/// single re-apply races the link still stabilizing, and drowsy firmware can
/// ACK a write yet drop it, leaving the wrong sensor DPI live until the *next*
/// nap. One confirming pass closes that hole without per-nap churn.
pub(super) const RECONNECT_REAPPLY_CONFIRM_RETRIES: u8 = 1;

/// Plan this refresh's volatile-settings writes: the [`reapply_targets`] set
/// plus a bounded run of confirming re-applies, and the follow-up keys (with
/// remaining retry counts) to confirm next refresh. `forced` re-applies to
/// every online device carrying the caller's confirm budget (system wake
/// keeps the boot ladder, a device reconnect the single link-race confirm);
/// first sightings always queue the full boot ladder, and plain
/// offline→online reconnects queue the link-race confirm.
pub(super) fn plan_reapply(
    prev: &[AgentDevice],
    next: &[AgentDevice],
    followup: &HashMap<String, u8>,
    forced: Option<u8>,
) -> (Vec<usize>, HashMap<String, u8>) {
    let mut targets = reapply_targets(prev, next, forced.is_some());
    let mut next_followup: HashMap<String, u8> = targets
        .iter()
        .filter_map(|&idx| {
            let id = stable_id(&next[idx]);
            let new_identity = !prev.iter().any(|p| stable_id(p) == id);
            let budget = if new_identity {
                VOLATILE_REAPPLY_CONFIRM_RETRIES
            } else if let Some(forced) = forced {
                forced
            } else {
                RECONNECT_REAPPLY_CONFIRM_RETRIES
            };
            // A new trigger can land mid-run (a reconnect while the boot
            // confirmations are still draining) — keep the larger of the
            // fresh budget and what this pass would have drained to.
            let carried = followup
                .get(&next[idx].config_key)
                .map_or(0, |remaining| remaining.saturating_sub(1));
            let budget = budget.max(carried);
            (budget > 0).then(|| (next[idx].config_key.clone(), budget))
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
