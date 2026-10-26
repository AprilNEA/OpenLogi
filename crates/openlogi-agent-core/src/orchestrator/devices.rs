//! The agent's device list: built from an inventory snapshot and ordered the
//! way the GUI carousel is, then diffed across ticks into the volatile-settings
//! re-apply plan, the host-switch links, and the follower cookie writes.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::config::{Config, DeviceConfig, ScrollResolution};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_hid::session::gesture::MainWheelDivert;
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute, Dpi, LeadCookie, ReportRate};

use super::AgentDevice;
use crate::hardware::WheelModeChange;
use crate::pointer_speed::SavedSpeed;
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

/// The saved press force to push to `dev`: only when the user saved one and
/// the probe found a force-sensing button whose force software may change,
/// so nothing is written to a device the user never set a force for.
pub(super) fn configured_force_sensing(config: &Config, dev: &AgentDevice) -> Option<u16> {
    dev.capabilities
        .is_some_and(|caps| caps.force_sensing)
        .then(|| config.force_sensing(&dev.config_key))
        .flatten()
}

/// The press-force writes a config reload owes: each online, enabled device
/// whose saved force differs from `previous`'s, with the force to write. A
/// removed force writes nothing — the device keeps the force it has — and a
/// disabled device is left alone, as pointer speed leaves it.
pub(super) fn changed_force_sensing(
    previous: &Config,
    config: &Config,
    devices: &[AgentDevice],
) -> Vec<(DeviceRoute, u16)> {
    devices
        .iter()
        .filter(|dev| dev.online && config.device_enabled(&dev.config_key))
        .filter_map(|dev| {
            let force = configured_force_sensing(config, dev)?;
            (Some(force) != configured_force_sensing(previous, dev))
                .then(|| Some((dev.route.clone()?, force)))
                .flatten()
        })
        .collect()
}

/// How precision mode sets `dev`'s speed, with the value saved for it, or
/// `None` when OpenLogi does not set its speed. A mouse with sensor DPI is a
/// DPI mouse; [`Capabilities::pointer_scaling`] is never set on one.
///
/// [`Capabilities::pointer_scaling`]: openlogi_core::device::Capabilities::pointer_scaling
pub(super) fn saved_speed(config: &Config, dev: &AgentDevice) -> Option<SavedSpeed> {
    let caps = dev.capabilities?;
    if caps.pointer {
        Some(SavedSpeed::Dpi(configured_dpi(config, dev)))
    } else if caps.pointer_scaling {
        Some(SavedSpeed::Scaled(config.pointer_speed(&dev.config_key)))
    } else {
        None
    }
}

/// The DPI saved for `dev` on its current link: what the volatile reapply
/// writes.
pub(super) fn configured_dpi(config: &Config, dev: &AgentDevice) -> Option<Dpi> {
    config
        .devices
        .get(dev.config_key.as_str())
        .and_then(|device| device.effective_dpi(&stable_id(dev).route_key()))
}

/// Whether the button assignments of the device `key` names differ between
/// `previous` and `config`: its bindings, its per-app bindings, or its
/// Actions Ring. Logi Options+ ends precision mode on such a change.
pub(super) fn assignments_changed(previous: &Config, config: &Config, key: &str) -> bool {
    // A device with no entry has the default assignments, so a reload that
    // only creates its entry (a first DPI preset, say) changes nothing.
    let empty = DeviceConfig::default();
    let before = previous.devices.get(key).unwrap_or(&empty);
    let after = config.devices.get(key).unwrap_or(&empty);
    before.bindings != after.bindings
        || before.per_app_bindings != after.per_app_bindings
        || before.action_ring != after.action_ring
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

/// Count one more settings change the agent made on `route` itself.
pub(super) fn bump_settings_epoch(epochs: &mut Vec<(DeviceRoute, u64)>, route: &DeviceRoute) {
    match epochs.iter_mut().find(|(known, _)| known == route) {
        Some((_, epoch)) => *epoch = epoch.wrapping_add(1),
        None => epochs.push((route.clone(), 1)),
    }
}

/// The inventory clients see: each paired device's reported settings revision
/// plus the agent's own changes on its route. Agent decisions keep the raw
/// snapshot, whose revision only a device report moves.
pub(super) fn published_inventories<'a>(
    inventories: &'a [DeviceInventory],
    epochs: &[(DeviceRoute, u64)],
) -> Cow<'a, [DeviceInventory]> {
    if epochs.is_empty() {
        return Cow::Borrowed(inventories);
    }
    Cow::Owned(
        inventories
            .iter()
            .map(|inventory| {
                let mut published = inventory.clone();
                for paired in &mut published.paired {
                    let epoch = DeviceRoute::for_slot(inventory, paired.slot).and_then(|route| {
                        epochs
                            .iter()
                            .find(|(known, _)| *known == route)
                            .map(|(_, epoch)| *epoch)
                    });
                    if let Some(epoch) = epoch {
                        paired.settings_revision = paired.settings_revision.wrapping_add(epoch);
                    }
                }
                published
            })
            .collect(),
    )
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

/// The `ChangeHost` cookie writes for one linked pointing device.
///
/// Logi Options+ moves a pointing device after a keyboard only when the
/// device's cookies name that keyboard. OpenLogi writes them for the links in
/// its config, so that Options+ on the keyboard's other hosts moves the same
/// devices. OpenLogi itself follows its config and never reads the cookies.
///
/// The clears run first. A link that moves from one keyboard to another is
/// therefore not refused because the old keyboard's byte is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FollowerCookieWrite {
    /// The config key of the linked pointing device. A clear that does not
    /// finish waits again under this key.
    pub(super) key: String,
    /// The linked pointing device.
    pub(super) follower: DeviceRoute,
    /// The keyboard bytes to clear.
    pub(super) release: Vec<LeadCookie>,
    /// The keyboard bytes to write after the clears.
    pub(super) link: Vec<LeadCookie>,
}

/// What the agent keeps between cookie plans.
///
/// - The held clears: the keyboard bytes that removed links left on devices
///   that were offline, or that a clear did not remove, by the config key of
///   each device. A clear runs when its device is next online.
/// - The known bytes: the last byte seen for each keyboard, by config key. A
///   keyboard that is asleep reports no unit ID, and a keyboard whose
///   receiver is unplugged is not in the inventory. A link to it is still
///   cleared or written with its known byte.
///
/// Both are kept in memory only. After an agent restart, a byte that was held
/// stays on the device until the user links and unlinks the pair again, and a
/// keyboard's byte is known again once the keyboard reports its unit ID.
#[derive(Debug, Default)]
pub(super) struct FollowerCookieState {
    held: BTreeMap<String, Vec<LeadCookie>>,
    known: BTreeMap<String, LeadCookie>,
}

impl FollowerCookieState {
    /// Remember the byte of each device in `devices` that reports its unit ID.
    pub(super) fn remember_leads(&mut self, devices: &[AgentDevice]) {
        for device in devices {
            if let Some(lead) = LeadCookie::for_unit_id(device.unit_id) {
                self.known.insert(device.config_key.clone(), lead);
            }
        }
    }

    /// The byte that names `keyboard`: from its unit ID in `devices`, or else
    /// the last byte seen for it.
    fn lead(&self, devices: &[AgentDevice], keyboard: &str) -> Option<LeadCookie> {
        lead_cookie(devices, keyboard).or_else(|| self.known.get(keyboard).copied())
    }

    /// The bytes of the keyboards that `pairs` links to `follower`.
    fn linked_leads(
        &self,
        pairs: &BTreeSet<(&str, &str)>,
        devices: &[AgentDevice],
        follower: &str,
    ) -> Vec<LeadCookie> {
        pairs
            .iter()
            .filter(|&&(_, linked)| linked == follower)
            .filter_map(|&(keyboard, _)| self.lead(devices, keyboard))
            .collect()
    }

    fn add(&mut self, follower: &str, lead: LeadCookie) {
        let leads = self.held.entry(follower.to_owned()).or_default();
        if !leads.contains(&lead) {
            leads.push(lead);
        }
    }

    fn forget(&mut self, follower: &str, lead: LeadCookie) {
        if let Some(leads) = self.held.get_mut(follower) {
            leads.retain(|held| *held != lead);
            if leads.is_empty() {
                self.held.remove(follower);
            }
        }
    }

    /// Keep the clears in `leads`, which did not finish on `follower`, until
    /// it is next online.
    pub(super) fn requeue(&mut self, follower: &str, leads: impl IntoIterator<Item = LeadCookie>) {
        for lead in leads {
            self.add(follower, lead);
        }
    }

    fn take(&mut self, follower: &str) -> Vec<LeadCookie> {
        self.held.remove(follower).unwrap_or_default()
    }

    /// The clears held for `follower`.
    #[cfg(test)]
    pub(super) fn held_for(&self, follower: &str) -> &[LeadCookie] {
        self.held.get(follower).map_or(&[], Vec::as_slice)
    }
}

/// The cookie writes of one plan, one [`FollowerCookieWrite`] per device.
#[derive(Default)]
struct CookiePlan(BTreeMap<String, FollowerCookieWrite>);

impl CookiePlan {
    fn entry(&mut self, follower: &str, route: DeviceRoute) -> &mut FollowerCookieWrite {
        self.0
            .entry(follower.to_owned())
            .or_insert_with(|| FollowerCookieWrite {
                key: follower.to_owned(),
                follower: route,
                release: Vec::new(),
                link: Vec::new(),
            })
    }

    fn release(&mut self, follower: &str, route: DeviceRoute, lead: LeadCookie) {
        let write = self.entry(follower, route);
        if !write.release.contains(&lead) {
            write.release.push(lead);
        }
    }

    fn link(&mut self, follower: &str, route: DeviceRoute, lead: LeadCookie) {
        let write = self.entry(follower, route);
        if !write.link.contains(&lead) {
            write.link.push(lead);
        }
    }

    /// The writes for `follower` when OpenLogi can write to it again: the
    /// clears that `state` holds for it, then a link for each keyboard that
    /// `pairs` links to it. A held clear for a keyboard that is linked again
    /// is dropped.
    fn follower_online(
        &mut self,
        pairs: &BTreeSet<(&str, &str)>,
        devices: &[AgentDevice],
        follower: &str,
        route: &DeviceRoute,
        state: &mut FollowerCookieState,
    ) {
        let linked = state.linked_leads(pairs, devices, follower);
        for lead in state.take(follower) {
            if !linked.contains(&lead) {
                self.release(follower, route.clone(), lead);
            }
        }
        for lead in linked {
            self.link(follower, route.clone(), lead);
        }
    }

    fn into_writes(self) -> Vec<FollowerCookieWrite> {
        self.0.into_values().collect()
    }
}

/// Each keyboard → linked device pair that `config` names, by config key.
fn host_switch_pairs(config: &Config) -> BTreeSet<(&str, &str)> {
    config
        .devices
        .iter()
        .flat_map(|(keyboard, settings)| {
            settings
                .host_switch_targets
                .iter()
                .filter(move |target| *target != keyboard)
                .map(move |target| (keyboard.as_str(), target.as_str()))
        })
        .collect()
}

/// The byte that names `keyboard`, when `devices` holds its unit ID. A
/// keyboard that is asleep reports no unit ID.
fn lead_cookie(devices: &[AgentDevice], keyboard: &str) -> Option<LeadCookie> {
    devices
        .iter()
        .find(|device| device.config_key == keyboard)
        .and_then(|device| LeadCookie::for_unit_id(device.unit_id))
}

/// The route of `follower` when OpenLogi can write to it now: it is enabled,
/// online, and has a route.
fn writable_follower(
    config: &Config,
    devices: &[AgentDevice],
    follower: &str,
) -> Option<DeviceRoute> {
    if !config.device_enabled(follower) {
        return None;
    }
    devices
        .iter()
        .find(|device| device.config_key == follower && device.online)?
        .route
        .clone()
}

/// The cookie writes that a config change asks for: clear each pair that
/// `config` removes, and link each pair that it adds.
///
/// A removed pair whose device is offline or disabled is held in `state`. A
/// byte that another keyboard still linked to the device shares is not
/// cleared. A device that `config` enables again gets the writes of a
/// reconnect, because the changes made while it was disabled were not
/// written.
pub(super) fn changed_follower_cookies(
    previous: &Config,
    config: &Config,
    devices: &[AgentDevice],
    state: &mut FollowerCookieState,
) -> Vec<FollowerCookieWrite> {
    let before = host_switch_pairs(previous);
    let after = host_switch_pairs(config);
    let mut plan = CookiePlan::default();
    for &(keyboard, follower) in before.difference(&after) {
        let Some(lead) = state.lead(devices, keyboard) else {
            continue;
        };
        if state
            .linked_leads(&after, devices, follower)
            .contains(&lead)
        {
            continue;
        }
        match writable_follower(config, devices, follower) {
            Some(route) => plan.release(follower, route, lead),
            None => state.add(follower, lead),
        }
    }
    for &(keyboard, follower) in after.difference(&before) {
        let Some(lead) = state.lead(devices, keyboard) else {
            continue;
        };
        state.forget(follower, lead);
        if let Some(route) = writable_follower(config, devices, follower) {
            plan.link(follower, route, lead);
        }
    }
    for dev in devices {
        let key = dev.config_key.as_str();
        if previous.device_enabled(key) {
            continue;
        }
        if let Some(route) = writable_follower(config, devices, key) {
            plan.follower_online(&after, devices, key, &route, state);
        }
    }
    plan.into_writes()
}

/// The cookie writes that `dev` coming online asks for. The clears that
/// `state` holds for `dev` run first. Then each pair that `dev` is part of
/// is linked, which fills a cookie that the firmware cleared when a slot was
/// paired again, as Options+ does when a linked device connects.
pub(super) fn reconnect_follower_cookies(
    config: &Config,
    devices: &[AgentDevice],
    dev: &AgentDevice,
    state: &mut FollowerCookieState,
) -> Vec<FollowerCookieWrite> {
    let pairs = host_switch_pairs(config);
    let mut plan = CookiePlan::default();
    if let Some(route) = writable_follower(config, devices, &dev.config_key) {
        plan.follower_online(&pairs, devices, &dev.config_key, &route, state);
    }
    // A keyboard that comes online fills the cookies of the devices it links.
    for &(keyboard, follower) in &pairs {
        if keyboard != dev.config_key {
            continue;
        }
        if let (Some(lead), Some(route)) = (
            state.lead(devices, keyboard),
            writable_follower(config, devices, follower),
        ) {
            plan.link(follower, route, lead);
        }
    }
    plan.into_writes()
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

/// Whether two snapshots still describe the same device lifecycle. Receiver
/// slots can be reused by another physical device without changing their
/// stable ID. Missing identity fields are not replacement evidence: a later
/// probe can enrich them while requests and confirmation budgets remain live.
pub(super) fn same_device_lifecycle(previous: &AgentDevice, next: &AgentDevice) -> bool {
    if previous.route != next.route
        || (previous.unit_id != [0; 4]
            && next.unit_id != [0; 4]
            && previous.unit_id != next.unit_id)
    {
        return false;
    }
    if let (DeviceIdentity::Serial(before), DeviceIdentity::Serial(after)) = (
        DeviceIdentity::from_parts(previous.serial.as_deref(), previous.unit_id),
        DeviceIdentity::from_parts(next.serial.as_deref(), next.unit_id),
    ) && before != after
    {
        return false;
    }
    stable_id(previous) == stable_id(next)
        || (previous.unit_id != [0; 4]
            && previous.unit_id == next.unit_id
            && (previous.route.is_some() || previous.slot == next.slot))
}

/// Carry remaining attempts across identity enrichment, but never across a
/// replacement or removal. Both confirmation schedulers use this transition.
pub(super) fn carry_lifecycle_budgets(
    previous: &[AgentDevice],
    next: &[AgentDevice],
    budgets: &BTreeMap<DeviceStableId, u8>,
) -> BTreeMap<DeviceStableId, u8> {
    next.iter()
        .filter_map(|device| {
            let old = previous
                .iter()
                .find(|old| same_device_lifecycle(old, device))?;
            let remaining = *budgets.get(&stable_id(old))?;
            Some((stable_id(device), remaining))
        })
        .collect()
}

/// Indices into `next` of devices whose volatile settings need re-applying:
/// a device whose stable identity is newly present (a first sighting, or a
/// replug that re-enumerated under a new identity — e.g. a Bolt device that
/// moved slots), or an offline→online transition (a reconnect after device
/// sleep); plus — after a system wake — every online device. Devices are
/// matched across ticks by [`same_device_lifecycle`]. Offline devices are never targeted
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
            match prev.iter().find(|p| same_device_lifecycle(p, dev)) {
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

/// How many explicit confirmation passes a first-sighted or wake-targeted
/// device keeps re-applying its volatile settings after the initial write. A
/// cold restart leaves a Bolt/Unifying mouse slow to enumerate — and a system
/// wake can enumerate a receiver whose mouse link is still re-establishing —
/// so the first write (and a single confirm) can both time out against a
/// still-booting device. Four confirmations are requested at two-second
/// intervals; only a delayed confirmation consumes an attempt.
pub(super) const VOLATILE_REAPPLY_CONFIRM_RETRIES: u8 = 4;

/// The shorter confirmation budget for a link re-establishment (a device nap
/// ending in an offline→online transition or a `0x1d4b` reconnect broadcast).
/// The device was already booted, so it needs no boot-race ladder — but its
/// single re-apply races the link still stabilizing, and drowsy firmware can
/// ACK a write yet drop it, leaving the wrong sensor DPI live until the *next*
/// nap. One confirming pass closes that hole without per-nap churn.
pub(super) const RECONNECT_REAPPLY_CONFIRM_RETRIES: u8 = 1;

/// Plan this refresh's volatile-settings writes: the [`reapply_targets`] set
/// plus a bounded run of confirming re-applies, and the follow-up identities
/// (with remaining retry counts) to confirm next refresh. `forced` re-applies
/// to every online device carrying the caller's confirm budget (system wake
/// keeps the boot ladder, a device reconnect the single link-race confirm);
/// first sightings always queue the full boot ladder, and plain
/// offline→online reconnects queue the link-race confirm.
pub(super) fn plan_reapply(
    prev: &[AgentDevice],
    next: &[AgentDevice],
    followup: &BTreeMap<DeviceStableId, u8>,
    forced: Option<u8>,
    confirm_followup: bool,
) -> (Vec<usize>, BTreeMap<DeviceStableId, u8>) {
    let followup = carry_lifecycle_budgets(prev, next, followup);
    let mut targets = reapply_targets(prev, next, forced.is_some());
    let mut next_followup: BTreeMap<DeviceStableId, u8> = targets
        .iter()
        .filter_map(|&idx| {
            let id = stable_id(&next[idx]);
            let new_identity = !prev.iter().any(|p| same_device_lifecycle(p, &next[idx]));
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
            let carried = if new_identity {
                0
            } else {
                followup.get(&id).map_or(0, |remaining| {
                    remaining.saturating_sub(u8::from(confirm_followup))
                })
            };
            let budget = budget.max(carried);
            (budget > 0).then_some((id, budget))
        })
        .collect();
    for (idx, dev) in next.iter().enumerate() {
        if dev.online
            && dev.route.is_some()
            && !targets.contains(&idx)
            && let id = stable_id(dev)
            && let Some(&remaining) = followup.get(&id)
        {
            if confirm_followup {
                targets.push(idx);
                if remaining > 1 {
                    next_followup.insert(id, remaining - 1);
                }
            } else {
                next_followup.insert(id, remaining);
            }
        }
    }
    (targets, next_followup)
}

/// Plan confirming writes for a failed or cancelled Disable Keys request. The
/// request's own bounded run is separate from [`plan_reapply`], because a manual
/// key-disable request must not replay unrelated volatile settings.
pub(super) fn plan_disabled_keys_recovery(
    devices: &[AgentDevice],
    recovery: &BTreeMap<DeviceStableId, u8>,
    confirm_recovery: bool,
) -> (Vec<usize>, BTreeMap<DeviceStableId, u8>) {
    let mut targets = Vec::new();
    let mut next_recovery = BTreeMap::new();
    for (idx, dev) in devices.iter().enumerate() {
        let id = stable_id(dev);
        let Some(&remaining) = recovery.get(&id) else {
            continue;
        };
        if !dev.online || dev.route.is_none() {
            continue;
        }
        if confirm_recovery {
            targets.push(idx);
            if remaining > 1 {
                next_recovery.insert(id, remaining - 1);
            }
        } else {
            next_recovery.insert(id, remaining);
        }
    }
    (targets, next_recovery)
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
