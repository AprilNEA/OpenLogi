//! Headless runtime state owned by the background agent.
//!
//! This is the agent-side counterpart to the GUI's `AppState` runtime half,
//! stripped of every UI-only concern (asset resolution, display names, the
//! DPI/SmartShift read caches, the carousel). It owns the shared `Arc`s the
//! CGEventTap hook and the HID++ gesture watcher read, and rebuilds them from a
//! [`Config`] plus the latest device inventory.
//!
//! Unlike the GUI, the agent never runs lazy DPI-capability discovery, so
//! [`DpiCycleState::capabilities`] stays `None` and presets cycle at their raw
//! (still valid) values — exactly the GUI's "window never opened" behaviour.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

use openlogi_core::config::{Config, ScrollResolution};
use openlogi_core::device::{Capabilities, DeviceInventory};
use openlogi_hid::{CaptureChannel, DeviceRoute};
use tracing::warn;

use crate::bindings::{bindings_for, gesture_bindings_for, oshook_gestures_for};
use crate::capture_plan::{DeviceCapturePlan, SharedCapturePlans, plan_for_device};
use crate::device_order::DeviceStableId;
use crate::hook_runtime::{HookMaps, SharedHookMaps};
use crate::ipc::InventoryHealth;
use crate::receiver_access::ReceiverAccess;
use crate::watchers::gesture::GestureBindings;
use crate::{DpiCycleState, DpiCycles};

/// The minimal per-device facts the agent needs: the config key (binding /
/// preset lookup), the HID++ route (DPI/SmartShift writes + capture target), and
/// the identity fields the canonical ordering keys on (so the no-selection
/// fallback agrees with the GUI carousel — see [`crate::device_order`]).
struct AgentDevice {
    config_key: String,
    model_key: String,
    route: Option<DeviceRoute>,
    slot: u8,
    serial: Option<String>,
    unit_id: [u8; 4],
    capabilities: Option<Capabilities>,
    /// Live link state from the inventory snapshot. An offline→online
    /// transition is a reconnect — the device may have power-cycled, so its
    /// volatile settings need re-applying (#189).
    online: bool,
}

/// The shared runtime handed to the hook and the gesture watcher. Every field
/// is an `Arc`, so cloning is cheap; the orchestrator rewrites the inner values
/// on each rebuild and the background threads observe them on their next read.
#[derive(Clone)]
pub struct SharedRuntime {
    /// The OS-hook callback's single-action + gesture maps, behind one lock so a
    /// rebuild publishes both atomically (see [`HookMaps`]). Also read by the
    /// gesture watcher for the thumb-wheel/DPI-button single actions.
    pub hook_maps: SharedHookMaps,
    pub gesture_bindings: GestureBindings,
    pub dpi_cycle: Arc<RwLock<DpiCycles>>,
    /// One capture plan per online device — what to divert and how to
    /// dispatch, keyed by the device the events arrive on. Carries each
    /// device's effective thumb-wheel sensitivity.
    pub capture_plans: SharedCapturePlans,
    pub capture_channel: CaptureChannel,
    /// Exclusive receiver access shared by HID++ capture and pairing. Capture
    /// and pairing must never open the same receiver HID node concurrently.
    pub receiver_access: ReceiverAccess,
}

/// Owns the config + device selection and keeps [`SharedRuntime`] in sync.
pub struct Orchestrator {
    config: Config,
    devices: Vec<AgentDevice>,
    current: usize,
    current_app: Option<String>,
    /// The latest inventory snapshot, kept so the IPC server can answer the
    /// GUI's `inventory()` polls without re-enumerating (the agent owns all
    /// device I/O). The enum keeps "nothing checked yet" and "enumeration
    /// broken" distinct from "checked and empty" — the IPC `status` reports
    /// the distinction (as [`InventoryHealth`]) so the GUI can tell them
    /// apart.
    inventory: InventoryState,
    /// Set after a system wake: devices may have power-cycled while their
    /// set/route/online state looks identical across the sleep gap, so the
    /// next refresh re-applies volatile settings to every online device.
    reapply_all_next_refresh: bool,
    /// Config keys of devices first sighted last refresh, due one confirming
    /// re-apply: the first write can race the device's own boot and be lost.
    reapply_followup: HashSet<String>,
    shared: SharedRuntime,
}

/// See [`Orchestrator::inventory`] (the field) — the agent-side superset of
/// the wire-level [`InventoryHealth`], carrying the snapshot itself.
enum InventoryState {
    /// No enumeration has completed yet; the device set is unknown.
    Pending,
    /// The latest completed snapshot — empty means "checked, no devices".
    Ready(Vec<DeviceInventory>),
    /// Enumeration has never succeeded (broken HID backend / dead watcher).
    Unavailable,
}

impl Orchestrator {
    /// Build from a loaded config. Creates the shared `Arc`s and seeds them
    /// from the config with no devices yet; the first inventory tick fills in
    /// the routes and presets.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let shared = SharedRuntime {
            hook_maps: Arc::new(RwLock::new(HookMaps::default())),
            gesture_bindings: Arc::new(RwLock::new(BTreeMap::new())),
            dpi_cycle: Arc::new(RwLock::new(DpiCycles::default())),
            capture_plans: Arc::new(RwLock::new(Vec::new())),
            capture_channel: Arc::new(RwLock::new(None)),
            receiver_access: ReceiverAccess::default(),
        };
        let orch = Self {
            config,
            devices: Vec::new(),
            current: 0,
            current_app: None,
            inventory: InventoryState::Pending,
            reapply_all_next_refresh: false,
            reapply_followup: HashSet::new(),
            shared,
        };
        orch.rebuild();
        orch
    }

    /// A cheap clone of the shared `Arc`s to hand to the watchers and hook.
    #[must_use]
    pub fn shared(&self) -> SharedRuntime {
        self.shared.clone()
    }

    fn current_key(&self) -> Option<&str> {
        self.devices
            .get(self.current)
            .map(|d| d.config_key.as_str())
    }

    /// Build the OS-hook callback's maps for `key` + foreground `app`. Both hook
    /// sub-maps are app-scoped (a per-app override can demote the gesture owner),
    /// so they're built together here and published under one lock — keeping
    /// `rebuild` and `set_current_app` from drifting into a half-populated write.
    fn hook_maps_for(&self, key: Option<&str>, app: Option<&str>) -> HookMaps {
        // A disabled selected device gets empty maps: the OS hook then passes
        // its events through untouched instead of applying remaps to a device
        // the user asked OpenLogi to leave alone.
        if key.is_some_and(|k| !self.config.device_enabled(k)) {
            return HookMaps::default();
        }
        HookMaps {
            bindings: bindings_for(&self.config, key, app),
            gestures: oshook_gestures_for(&self.config, key, app),
        }
    }

    /// Rewrite every shared map from the current config + selected device.
    fn rebuild(&self) {
        let key = self.current_key();
        // One write publishes both hook maps atomically, so a button press during
        // an owner switch can't observe a half-updated state.
        write_value(
            &self.shared.hook_maps,
            self.hook_maps_for(key, self.current_app.as_deref()),
            "hook_maps",
        );
        write_value(
            &self.shared.gesture_bindings,
            gesture_bindings_for(&self.config, key),
            "gesture_bindings",
        );
        self.publish_device_runtime();
    }

    /// Republish the runtime views derived from the device set + config: the
    /// capture plans and the per-device DPI-cycle map. One method so the
    /// inventory fast path (same set, fresh online flags) can't update one and
    /// forget the other — a waking device needs both its capture session and
    /// its DPI-cycle slot.
    fn publish_device_runtime(&self) {
        write_value(
            &self.shared.capture_plans,
            self.capture_plans_for(),
            "capture_plans",
        );
        self.rebuild_dpi_cycles(self.current_key());
    }

    /// Rewrite the per-device DPI-cycle map for every online device,
    /// preserving a device's live cycle index (and lazily discovered
    /// capabilities) across rebuilds whose presets did not change — a config
    /// reload must not snap DPI back to `preset[0]`.
    fn rebuild_dpi_cycles(&self, selected: Option<&str>) {
        let Ok(mut guard) = self.shared.dpi_cycle.write() else {
            warn!("dpi_cycle lock poisoned — rebuild skipped");
            return;
        };
        let mut by_key = std::collections::HashMap::new();
        for dev in self
            .devices
            .iter()
            .filter(|dev| dev.online && self.config.device_enabled(&dev.config_key))
        {
            let Some(route) = dev.route.clone() else {
                continue;
            };
            let presets = self.config.dpi_presets(&dev.config_key);
            let previous = guard
                .by_key
                .get(&dev.config_key)
                .filter(|state| state.presets == presets);
            by_key.insert(
                dev.config_key.clone(),
                DpiCycleState {
                    index: previous.map_or(0, |state| state.index),
                    capabilities: previous.and_then(|state| state.capabilities.clone()),
                    presets,
                    target: Some(route),
                },
            );
        }
        guard.selected = selected.map(str::to_owned);
        guard.by_key = by_key;
    }

    /// One capture plan per online device, from the current config + app.
    fn capture_plans_for(&self) -> Vec<DeviceCapturePlan> {
        self.devices
            .iter()
            .filter(|dev| dev.online && self.config.device_enabled(&dev.config_key))
            .filter_map(|dev| {
                let route = dev.route.clone()?;
                Some(plan_for_device(
                    &self.config,
                    &dev.config_key,
                    route,
                    self.current_app.as_deref(),
                ))
            })
            .collect()
    }

    /// Apply a fresh inventory snapshot. Always refreshes the snapshot the IPC
    /// `inventory()` poll serves (battery / online state changes without
    /// altering the device *set*), but only re-picks the selection and rebuilds
    /// the shared maps when the device set actually changed — `rebuild()` is
    /// driven solely by `config_key` + route and resets the live DPI-cycle
    /// index, so running it every 2s tick on an unchanged set would snap DPI
    /// back to `preset[0]` (and burn three `RwLock` writes) for nothing.
    pub fn refresh_inventory(&mut self, inventories: &[DeviceInventory]) {
        // Even an empty snapshot is a *completed* enumeration — the watcher
        // skips failed ticks — so the device set is now known either way (and
        // a recovered backend upgrades `Unavailable` back to live data).
        self.inventory = InventoryState::Ready(inventories.to_vec());
        let devices = build_devices(inventories);
        // Volatile settings (lighting colour, sensor DPI, SmartShift, native
        // wheel mode) live in device RAM and reset on a power cycle. Every
        // reconnect shape re-applies the persisted values (#189): a first
        // sighting, a replug (new route), a wake from device sleep
        // (offline→online), or — via the
        // flag — a system wake where none of those are observable.
        let reapply_all = std::mem::take(&mut self.reapply_all_next_refresh);
        let followup = std::mem::take(&mut self.reapply_followup);
        let (targets, next_followup) =
            plan_reapply(&self.devices, &devices, &followup, reapply_all);
        self.reapply_followup = next_followup;
        for idx in targets {
            self.reapply_volatile_settings(&devices[idx]);
        }
        let changed = devices.len() != self.devices.len()
            || devices.iter().zip(&self.devices).any(|(a, b)| {
                a.config_key != b.config_key
                    || a.route != b.route
                    || a.capabilities != b.capabilities
            });
        if !changed {
            // Same set and routes — but keep the fresh `online` flags, or a
            // device that woke this tick would read as a transition forever.
            // The runtime views key on `online`, so republish them even here or
            // a woken device would get neither its capture session nor its
            // DPI-cycle slot.
            self.devices = devices;
            self.publish_device_runtime();
            return;
        }
        self.devices = devices;
        self.current = pick_current(&self.devices, self.config.selected_device());
        self.rebuild();
    }

    /// Force a volatile-settings re-apply for every online device on the next
    /// inventory refresh. Called on a detected system wake: the devices were
    /// likely power-cycled during the sleep, but the first post-wake snapshot
    /// can look identical to the last pre-sleep one (same set, same routes,
    /// already online), so the per-device transition triggers never fire.
    pub fn reapply_volatile_on_next_refresh(&mut self) {
        self.reapply_all_next_refresh = true;
    }

    /// Push the persisted volatile settings (lighting, sensor DPI, SmartShift,
    /// native wheel mode) to one device, fire-and-forget on background threads.
    /// Reuses the capture session's channel when it already points at the
    /// device, like every other hardware write.
    fn reapply_volatile_settings(&self, dev: &AgentDevice) {
        // A disabled device is left fully native — no writes of any kind.
        if !self.config.device_enabled(&dev.config_key) {
            return;
        }
        let Some(route) = dev.route.clone() else {
            return;
        };
        let key = &dev.config_key;
        let (resolution, inverted) = configured_wheel_mode(&self.config, dev);
        crate::hardware::write_scroll_wheel_mode_in_background(
            Some(&self.shared.capture_channel),
            (resolution.is_some() || inverted.is_some()).then_some(route.clone()),
            resolution,
            inverted,
        );
        if let Some(lighting) = self.config.lighting(key).filter(|l| l.enabled) {
            crate::hardware::set_lighting_in_background(Some(route.clone()), &lighting);
        }
        if let Some(dpi) = self.config.dpi(key) {
            crate::hardware::write_dpi_in_background(
                Some(&self.shared.capture_channel),
                Some(route.clone()),
                dpi,
            );
        }
        if let Some(smartshift) = self.config.smartshift(key) {
            crate::hardware::write_smartshift_in_background(
                Some(&self.shared.capture_channel),
                Some(route),
                smartshift.mode.into(),
                smartshift.auto_disengage,
                smartshift.tunable_torque,
            );
        }
    }

    /// Push the saved native wheel resolution/inversion to every currently online
    /// device. Separated from [`Self::rebuild`] (which also runs on
    /// foreground-app changes) because the HID++ write is only needed when
    /// config or device presence changes. The write short-circuits at the
    /// `0x2121` layer when the wheel already holds the desired state, so calling
    /// it on every reload costs at most one wheel-mode read per device — and
    /// still recovers a device whose earlier write timed out while it was waking.
    fn apply_native_wheel_modes(&self) {
        for dev in self
            .devices
            .iter()
            .filter(|dev| dev.online && dev.route.is_some())
        {
            let (resolution, inverted) = configured_wheel_mode(&self.config, dev);
            crate::hardware::write_scroll_wheel_mode_in_background(
                Some(&self.shared.capture_channel),
                (resolution.is_some() || inverted.is_some())
                    .then(|| dev.route.clone())
                    .flatten(),
                resolution,
                inverted,
            );
        }
    }

    /// The latest inventory snapshot (for the IPC `inventory()` poll). Empty
    /// until the first enumeration completes — pair it with
    /// [`Self::inventory_health`] to tell "unknown" from "none".
    #[must_use]
    pub fn inventory(&self) -> Vec<DeviceInventory> {
        match &self.inventory {
            InventoryState::Ready(inventories) => inventories.clone(),
            InventoryState::Pending | InventoryState::Unavailable => Vec::new(),
        }
    }

    /// Where enumeration stands, for the IPC `status` poll.
    #[must_use]
    pub fn inventory_health(&self) -> InventoryHealth {
        match self.inventory {
            InventoryState::Pending => InventoryHealth::Scanning,
            InventoryState::Ready(_) => InventoryHealth::Ready,
            InventoryState::Unavailable => InventoryHealth::Unavailable,
        }
    }

    /// Record that enumeration has never worked and has stopped being treated
    /// as "still starting" (persistent initial failure, or the watcher died).
    /// Downgrades only [`InventoryState::Pending`]: once a snapshot exists the
    /// last good device set stays authoritative — the same policy as the
    /// watcher skipping failed mid-session ticks.
    pub fn mark_inventory_unavailable(&mut self) {
        if matches!(self.inventory, InventoryState::Pending) {
            self.inventory = InventoryState::Unavailable;
        }
    }

    /// Whether autostart is enabled in the current config (for IPC `status`).
    #[must_use]
    pub fn launch_at_login(&self) -> bool {
        self.config.app_settings.launch_at_login
    }

    /// Foreground-app change → re-overlay per-app bindings on the hook maps (DPI
    /// and the dedicated HID++ gesture map are not app-scoped, so they're untouched).
    /// Both hook maps are recomputed: a per-app override of the gesture owner
    /// turns it into a single action for that app, dropping it from the OS-hook
    /// gesture set — so the gesture map is app-scoped too.
    pub fn set_current_app(&mut self, bundle: Option<String>) {
        if bundle == self.current_app {
            return;
        }
        self.current_app = bundle;
        write_value(
            &self.shared.hook_maps,
            self.hook_maps_for(self.current_key(), self.current_app.as_deref()),
            "hook_maps",
        );
    }

    /// Replace the config (after `config.toml` changed) and rebuild everything.
    pub fn reload_config(&mut self, config: Config) {
        self.config = config;
        self.current = pick_current(&self.devices, self.config.selected_device());
        self.rebuild();
        self.apply_native_wheel_modes();
    }
}

/// Resolve the two independently-gated HiResWheel settings for one device.
/// `None` means preserve the device's current value.
fn configured_wheel_mode(
    config: &Config,
    dev: &AgentDevice,
) -> (Option<ScrollResolution>, Option<bool>) {
    let Some(capabilities) = dev.capabilities else {
        return (None, None);
    };
    let resolution = capabilities
        .hires_wheel
        .then(|| config.scroll_resolution(&dev.config_key))
        .flatten();
    let inverted = capabilities
        .scroll_inversion
        .then(|| config.invert_scroll(&dev.config_key));
    (resolution, inverted)
}

/// Build the agent device list from an inventory snapshot. Mirrors the GUI's
/// `build_device_list` minus the asset/display fields: a device is included
/// only once its HID++ DeviceInformation (`model_info`) has resolved, since the
/// model key is derived from it.
fn build_devices(inventories: &[DeviceInventory]) -> Vec<AgentDevice> {
    let mut devices = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            let route = DeviceRoute::device_route_for(inv, paired.slot);
            let stable_id = DeviceStableId::from_parts(
                route.as_ref(),
                paired.slot,
                model.serial_number.as_deref(),
                model.unit_id,
            );
            devices.push(AgentDevice {
                config_key: stable_id.config_key(),
                model_key: model.config_key(),
                route,
                slot: paired.slot,
                serial: model.serial_number.clone(),
                unit_id: model.unit_id,
                capabilities: paired.capabilities,
                online: paired.online,
            });
        }
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

/// The canonical identity of one device: what the GUI carousel orders by, what
/// the config key is derived from, and what [`reapply_targets`] matches a device
/// against across inventory ticks.
fn stable_id(dev: &AgentDevice) -> DeviceStableId {
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
fn reapply_targets(prev: &[AgentDevice], next: &[AgentDevice], reapply_all: bool) -> Vec<usize> {
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

/// Plan this refresh's volatile-settings writes: the [`reapply_targets`] set
/// plus one confirming re-apply for devices first sighted last refresh, and
/// the follow-up keys to confirm next refresh.
fn plan_reapply(
    prev: &[AgentDevice],
    next: &[AgentDevice],
    followup: &HashSet<String>,
    reapply_all: bool,
) -> (Vec<usize>, HashSet<String>) {
    let mut targets = reapply_targets(prev, next, reapply_all);
    let next_followup = targets
        .iter()
        .filter(|&&idx| {
            let id = stable_id(&next[idx]);
            !prev.iter().any(|p| stable_id(p) == id)
        })
        .map(|&idx| next[idx].config_key.clone())
        .collect();
    for (idx, dev) in next.iter().enumerate() {
        if dev.online
            && dev.route.is_some()
            && followup.contains(&dev.config_key)
            && !targets.contains(&idx)
        {
            targets.push(idx);
        }
    }
    (targets, next_followup)
}

/// Index of the selected device: the one whose `config_key` matches the saved
/// selection, else the first. `build_devices` sorts by the same canonical key
/// the GUI carousel uses, so "the first" is the same physical device in both
/// processes even when nothing is persisted yet.
fn pick_current(devices: &[AgentDevice], saved: Option<&str>) -> usize {
    saved
        .and_then(|key| devices.iter().position(|d| d.config_key == key))
        .unwrap_or(0)
}

/// Replace the value behind an `RwLock`, logging (not panicking) on poison so a
/// background thread that paniced while holding the lock can't take the agent
/// down — it just keeps the stale value until the next successful rebuild.
fn write_value<T>(lock: &RwLock<T>, value: T, name: &str) {
    match lock.write() {
        Ok(mut guard) => *guard = value,
        Err(e) => warn!(error = %e, lock = name, "lock poisoned — keeping stale value"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentDevice, InventoryHealth, Orchestrator, configured_wheel_mode, plan_reapply,
        reapply_targets,
    };
    use openlogi_core::config::{Config, ScrollResolution};
    use openlogi_core::device::Capabilities;
    use openlogi_hid::DeviceRoute;

    fn dev(key: &str, slot: u8, online: bool) -> AgentDevice {
        AgentDevice {
            config_key: key.to_string(),
            model_key: key.to_string(),
            route: Some(DeviceRoute::Bolt {
                receiver_uid: "AA00".to_string(),
                slot,
            }),
            slot,
            serial: None,
            unit_id: [0; 4],
            capabilities: None,
            online,
        }
    }

    #[test]
    fn configured_wheel_mode_gates_resolution_and_inversion_independently() {
        let mut config = Config::default();
        config.set_scroll_resolution("a", Some(ScrollResolution::Low));
        config.set_invert_scroll("a", true);
        let mut device = dev("a", 1, true);

        device.capabilities = Some(Capabilities {
            hires_wheel: true,
            scroll_inversion: false,
            ..Capabilities::default()
        });
        assert_eq!(
            configured_wheel_mode(&config, &device),
            (Some(ScrollResolution::Low), None)
        );

        device.capabilities = Some(Capabilities {
            hires_wheel: false,
            scroll_inversion: true,
            ..Capabilities::default()
        });
        assert_eq!(configured_wheel_mode(&config, &device), (None, Some(true)));

        device.capabilities = None;
        assert_eq!(configured_wheel_mode(&config, &device), (None, None));
    }

    #[test]
    fn configured_wheel_mode_leaves_unset_resolution_unmanaged() {
        let config = Config::default();
        let mut device = dev("a", 1, true);
        device.capabilities = Some(Capabilities {
            hires_wheel: true,
            scroll_inversion: false,
            ..Capabilities::default()
        });

        assert_eq!(configured_wheel_mode(&config, &device), (None, None));
    }

    #[test]
    fn reapply_targets_new_arrivals_and_transitions() {
        // First sighting of an online device → re-apply.
        assert_eq!(reapply_targets(&[], &[dev("a", 1, true)], false), vec![0]);
        // Steady state → nothing.
        assert!(reapply_targets(&[dev("a", 1, true)], &[dev("a", 1, true)], false).is_empty());
        // Replug under a new route (same key, new slot) → re-apply.
        assert_eq!(
            reapply_targets(&[dev("a", 1, true)], &[dev("a", 2, true)], false),
            vec![0]
        );
        // Waking from device sleep (offline → online) → re-apply.
        assert_eq!(
            reapply_targets(&[dev("a", 1, false)], &[dev("a", 1, true)], false),
            vec![0]
        );
        // Going to sleep (online → offline) → nothing.
        assert!(reapply_targets(&[dev("a", 1, true)], &[dev("a", 1, false)], false).is_empty());
    }

    #[test]
    fn reapply_targets_disambiguates_same_model_duplicates() {
        // Two devices can share a model key but are distinct physical units at
        // different Bolt slots, so they have distinct stable ids. A steady tick
        // with both already online must target NEITHER.
        let prev = [dev("dup", 1, true), dev("dup", 2, true)];
        let next = [dev("dup", 1, true), dev("dup", 2, true)];
        assert!(reapply_targets(&prev, &next, false).is_empty());
    }

    #[test]
    fn reapply_targets_skip_offline_and_routeless_devices() {
        // A paired-but-asleep new arrival waits for its online transition —
        // writing now would only time out against a sleeping device.
        assert!(reapply_targets(&[], &[dev("a", 1, false)], false).is_empty());
        let routeless = AgentDevice {
            route: None,
            ..dev("b", 2, true)
        };
        assert!(reapply_targets(&[], &[routeless], false).is_empty());
    }

    #[test]
    fn reapply_all_targets_every_online_device() {
        let prev = [dev("a", 1, true), dev("b", 2, false)];
        let next = [dev("a", 1, true), dev("b", 2, false)];
        // The post-wake snapshot looks identical to the pre-sleep one; the
        // flag still re-applies to the online device (and only that one).
        assert_eq!(reapply_targets(&prev, &next, true), vec![0]);
    }

    #[test]
    fn plan_reapply_confirms_a_first_sighting_once() {
        use std::collections::HashSet;
        // First sighting: applied now, queued for one confirming re-apply.
        let (targets, followup) = plan_reapply(&[], &[dev("a", 1, true)], &HashSet::new(), false);
        assert_eq!(targets, vec![0]);
        assert_eq!(followup, HashSet::from(["a".to_string()]));
        // Next refresh: the confirming apply fires, then the queue drains.
        let prev = [dev("a", 1, true)];
        let (targets, followup) = plan_reapply(&prev, &prev, &followup, false);
        assert_eq!(targets, vec![0]);
        assert!(followup.is_empty());
        // Steady state after that: nothing.
        let (targets, _) = plan_reapply(&prev, &prev, &followup, false);
        assert!(targets.is_empty());
    }

    #[test]
    fn plan_reapply_transitions_are_not_queued_for_confirmation() {
        use std::collections::HashSet;
        // A wake from device sleep re-applies once — the device was already
        // booted, so no confirming write is queued.
        let (targets, followup) = plan_reapply(
            &[dev("a", 1, false)],
            &[dev("a", 1, true)],
            &HashSet::new(),
            false,
        );
        assert_eq!(targets, vec![0]);
        assert!(followup.is_empty());
    }

    #[test]
    fn plan_reapply_skips_a_followup_that_went_offline() {
        use std::collections::HashSet;
        let prev = [dev("a", 1, true)];
        let (targets, followup) = plan_reapply(
            &prev,
            &[dev("a", 1, false)],
            &HashSet::from(["a".to_string()]),
            false,
        );
        assert!(targets.is_empty());
        assert!(followup.is_empty());
    }

    /// An *empty* snapshot still flips the health to `Ready`: the watcher only
    /// forwards completed enumerations, so "checked and found nothing" must not
    /// be reported as "still scanning" — that's the whole distinction the
    /// health exists to carry.
    #[test]
    fn empty_refresh_marks_inventory_ready() {
        let mut orch = Orchestrator::new(Config::default());
        assert_eq!(orch.inventory_health(), InventoryHealth::Scanning);
        orch.refresh_inventory(&[]);
        assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
    }

    /// `Unavailable` is a startup-only downgrade: it reports "enumeration has
    /// never worked", recovers when a snapshot finally lands, and never
    /// clobbers a live device set on a mid-session failure (mirroring the
    /// watcher's keep-last-snapshot policy).
    #[test]
    fn unavailable_only_downgrades_a_pending_inventory() {
        let mut orch = Orchestrator::new(Config::default());
        orch.mark_inventory_unavailable();
        assert_eq!(orch.inventory_health(), InventoryHealth::Unavailable);
        orch.refresh_inventory(&[]);
        assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
        orch.mark_inventory_unavailable();
        assert_eq!(orch.inventory_health(), InventoryHealth::Ready);
    }
}
