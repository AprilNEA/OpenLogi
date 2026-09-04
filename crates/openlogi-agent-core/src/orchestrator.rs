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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{Action, Binding};
use openlogi_core::bindings::{button_bindings_for, oshook_gestures_for};
use openlogi_core::config::{Config, LightSettings, ScrollResolution, canonical_device_key};
use openlogi_core::device::{
    Capabilities, DeviceInventory, DeviceKind, LightCapabilities, StandaloneDevice,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};
use openlogi_hid::{
    CaptureChannel, ChannelPool, ChannelRegistry, DIRECT_DEVICE_INDEX, DeviceIoGate, DeviceRoute,
    KEYBOARD_KEY_CIDS,
};
use openlogi_ipc::InventoryHealth;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::action_ring::ActionRingSessionSpec;
use crate::capture_plan::{
    DeviceCaptureIdentity, DeviceCapturePlan, SharedCapturePlans, hidpp_side_gesture_maps_for,
    plan_for_device,
};
use crate::hardware::DeviceOp;
use crate::observable::ObservableState;
use crate::receiver_access::ReceiverAccess;
use crate::runtime::hook::{HookMaps, SharedHookMaps};
use crate::runtime::scroll::ScrollPreferences;
use crate::watchers::host_switch::{HostSwitchLink, HostSwitchLinks};
use crate::watchers::keyboard::{KeyboardSpec, SharedKeyboardSpec};
use crate::{DpiCycleState, DpiCycles};

/// The minimal per-device facts the agent needs: the config key (binding /
/// preset lookup), the HID++ route (DPI/SmartShift writes + capture target), and
/// the identity fields the canonical ordering keys on (so the no-selection
/// fallback agrees with the GUI carousel — see [`openlogi_core::device_order`]).
struct AgentDevice {
    config_key: String,
    /// `DeviceModelInfo::config_key()` for a receiver-paired/HID++ device —
    /// what `capture_plan::spy_buttons_for_model` keys its lookup on. The
    /// standalone-device constructor below instead fills this with
    /// `display_name`, which can never match a `SPY_BUTTON_MAPS` entry — a
    /// latent mismatch, harmless today because standalone enumeration is
    /// Litra-only (never `Capabilities::gaming_buttons`), but real once a
    /// gaming device is ever reachable through the standalone/raw-HID path.
    model_key: String,
    route: Option<DeviceRoute>,
    slot: u8,
    serial: Option<String>,
    unit_id: [u8; 4],
    capabilities: Option<Capabilities>,
    /// HID++-reported device kind — identity only (capability decisions come
    /// from the feature table). Used to find the keyboard the key-capture
    /// watcher should target.
    kind: DeviceKind,
    light_capabilities: Option<LightCapabilities>,
    /// Live link state from the inventory snapshot. An offline→online
    /// transition is a reconnect — the device may have power-cycled, so its
    /// volatile settings need re-applying (#189).
    online: bool,
}

/// Cheaply cloneable runtime handles handed to hooks and background managers.
/// The orchestrator remains the sole producer for its watch-backed projections;
/// consumers receive only read capabilities through this type.
#[derive(Clone)]
pub struct SharedRuntime {
    /// The OS-hook callback's single-action + gesture maps, behind one lock so a
    /// rebuild publishes both atomically (see [`HookMaps`]). Also read by the
    /// gesture watcher for the thumb-wheel/DPI-button single actions.
    pub hook_maps: SharedHookMaps,
    /// Function-key remapper bindings (keycode+modifiers → action). Not
    /// per-app-profile in M1 (spec non-goal), so a single shared map.
    pub keyboard_bindings: crate::runtime::hook::SharedKeyboardBindings,
    /// Live smooth-scroll and vertical sensitivity settings read without
    /// taking the orchestrator/config lock.
    pub scroll_preferences: Arc<ScrollPreferences>,
    pub dpi_cycle: Arc<RwLock<DpiCycles>>,
    /// One capture plan per online device — what to divert and how to
    /// dispatch, keyed by the device the events arrive on. Carries each
    /// device's effective thumb-wheel sensitivity.
    pub capture_plans: SharedCapturePlans,
    pub capture_channel: CaptureChannel,
    /// Exact-route channels owned and published by the inventory enumerator.
    pub channel_registry: ChannelRegistry,
    /// Host-lifecycle gate shared by every producer of proactive device I/O.
    pub device_io: DeviceIoGate,
    /// Shared transport pool used by long-running host-switch sessions.
    pub channel_pool: ChannelPool,
    /// The keyboard key-capture watcher's target + bindings, `None` while no
    /// online keyboard has bound keys.
    pub keyboard_spec: SharedKeyboardSpec,
    /// The keyboard capture session's open channel, reused by Fn-lock writes
    /// (the mouse-oriented [`Self::capture_channel`] points elsewhere).
    pub keyboard_channel: CaptureChannel,
    /// Incremented when a device reconnects or the system wakes, so capture
    /// sessions re-arm volatile HID++ control diversion even when route and
    /// online flags look unchanged.
    pub capture_rearm_generation: Arc<AtomicU64>,
    /// Receiver access shared by HID++ sessions and pairing. Pairing/host
    /// transitions are exclusive; capture sessions share under read leases.
    pub receiver_access: ReceiverAccess,
    /// Keyboard → pointing-device routes resolved from `config.toml`.
    pub host_switch_links: HostSwitchLinks,
}

impl SharedRuntime {
    /// Bind a device operation to `route` on the mouse/pointer capture
    /// channel — the registry-confirmed capture channel or the exact current
    /// inventory channel that every device write already resolves through.
    #[must_use]
    pub fn device(&self, route: &DeviceRoute) -> DeviceOp<'_> {
        DeviceOp::new(
            &self.capture_channel,
            &self.channel_registry,
            &self.receiver_access,
            &self.device_io,
            route,
        )
    }

    /// Same, but against the keyboard capture channel — Fn-lock writes run on
    /// the keyboard's own capture session, not the mouse-oriented
    /// [`Self::capture_channel`].
    #[must_use]
    pub fn keyboard_device(&self, route: &DeviceRoute) -> DeviceOp<'_> {
        DeviceOp::new(
            &self.keyboard_channel,
            &self.channel_registry,
            &self.receiver_access,
            &self.device_io,
            route,
        )
    }
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
    /// Whether the last enumeration pass failed to open HID++ nodes; published
    /// atomically with the inventory so no observation pairs a fresh device
    /// set with a stale flag.
    hid_open_failures: bool,
    /// Config keys of devices first sighted (or targeted after wake) recently,
    /// with remaining confirming re-apply budget: the first write can race the
    /// device's own boot or reconnect and be lost.
    reapply_followup: HashMap<String, u8>,
    /// Last successful aggregate camera-use sample. `None` means the macOS
    /// watcher has not produced its first usable observation yet.
    camera_active: Option<bool>,
    /// Transient manual power choices for camera-linked lights. A camera-use
    /// transition clears them; they are never written to the config.
    manual_light_overrides: BTreeMap<String, bool>,
    /// Whether the OS mouse hook is currently installed. Back/Forward gesture
    /// motion comes from HID++, but diversion is published only while the
    /// broader mouse-remapping path is available so losing the hook leaves the
    /// side buttons native.
    os_mouse_hook_available: bool,
    /// Private producer halves for the read-only runtime projections in
    /// `shared`, keeping the orchestrator's single-writer contract structural.
    capture_plans_tx: watch::Sender<Arc<Vec<DeviceCapturePlan>>>,
    keyboard_spec_tx: watch::Sender<Option<Arc<KeyboardSpec>>>,
    host_switch_links_tx: watch::Sender<Arc<Vec<HostSwitchLink>>>,
    shared: SharedRuntime,
    /// The state the GUI observes. Every mutator below that changes one of its
    /// facts republishes here, so the cell cannot go stale behind a new code
    /// path — see [`ObservableState`].
    observable: Arc<ObservableState>,
}

/// See [`Orchestrator::inventory`] (the field) — the agent-side superset of
/// the wire-level [`InventoryHealth`], carrying the snapshot itself.
enum InventoryState {
    /// No enumeration has completed yet; the device set is unknown.
    Pending,
    /// The latest completed snapshot — empty means "checked, no devices".
    Ready {
        inventories: Vec<DeviceInventory>,
        standalone: Vec<StandaloneDevice>,
    },
    /// Enumeration has never succeeded (broken HID backend / dead watcher).
    Unavailable,
}

impl Orchestrator {
    /// Build from a loaded config. Creates the shared runtime handles and seeds
    /// them from the config with no devices yet; the first inventory tick fills
    /// in the routes and presets.
    ///
    /// `observable` is the cell the IPC server answers from; the config facts
    /// it carries are seeded here.
    #[must_use]
    pub fn new(config: Config, observable: Arc<ObservableState>) -> Self {
        let (capture_plans_tx, capture_plans) = watch::channel(Arc::new(Vec::new()));
        let (keyboard_spec_tx, keyboard_spec) = watch::channel(None);
        let (host_switch_links_tx, host_switch_links) = watch::channel(Arc::new(Vec::new()));
        let shared = SharedRuntime {
            hook_maps: Arc::new(RwLock::new(HookMaps::default())),
            keyboard_bindings: Arc::new(RwLock::new(config.keyboard.bindings.clone())),
            scroll_preferences: Arc::new(ScrollPreferences::new(
                config.app_settings.smooth_scroll,
                config.app_settings.vertical_scroll_sensitivity,
            )),
            dpi_cycle: Arc::new(RwLock::new(DpiCycles::default())),
            capture_plans,
            capture_channel: Arc::new(RwLock::new(None)),
            channel_registry: ChannelRegistry::default(),
            device_io: openlogi_hid::host::device_io_gate(),
            channel_pool: openlogi_hid::host::channel_pool(),
            keyboard_spec,
            keyboard_channel: Arc::new(RwLock::new(None)),
            capture_rearm_generation: Arc::new(AtomicU64::new(0)),
            receiver_access: ReceiverAccess::default(),
            host_switch_links,
        };
        let orch = Self {
            config,
            devices: Vec::new(),
            current: 0,
            current_app: None,
            inventory: InventoryState::Pending,
            reapply_all_next_refresh: false,
            hid_open_failures: false,
            reapply_followup: HashMap::new(),
            camera_active: None,
            manual_light_overrides: BTreeMap::new(),
            os_mouse_hook_available: false,
            capture_plans_tx,
            keyboard_spec_tx,
            host_switch_links_tx,
            shared,
            observable,
        };
        orch.rebuild();
        orch.observable
            .set_launch_at_login(orch.config.app_settings.launch_at_login);
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
            .filter(|device| is_hidpp_device(device))
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
        let mut bindings = button_bindings_for(&self.config, key, app);
        let mut gestures = oshook_gestures_for(&self.config, key, app);
        if let Some(key) = key {
            for button in hidpp_side_gesture_maps_for(&self.config, key, app).keys() {
                // HID++ owns both edges for these controls. Keeping their
                // projected click or gesture map in the global hook would
                // reintroduce a second, unattributed dispatch path.
                bindings.remove(button);
                gestures.remove(button);
            }
        }
        HookMaps {
            bindings,
            gestures,
            selected_device: key.map(str::to_owned),
            ..HookMaps::default()
        }
    }

    /// Publish hook maps while preserving thumb-wheel polarities learned from
    /// hardware capture sessions. Selection, polarity, and bindings share the
    /// one lock the callback reads, so a device switch cannot combine facts
    /// from two devices.
    fn publish_hook_maps(&self, mut maps: HookMaps) {
        match self.shared.hook_maps.write() {
            Ok(mut current) => {
                maps.thumbwheel_positive_is_forward =
                    std::mem::take(&mut current.thumbwheel_positive_is_forward);
                *current = maps;
            }
            Err(error) => {
                warn!(%error, lock = "hook_maps", "lock poisoned — keeping stale value");
            }
        }
    }

    /// The keyboard key-capture spec for the first known keyboard, or `None`
    /// when no keyboard is paired or none of its capturable keys carries a
    /// real binding (an unbound key must never be diverted).
    ///
    /// Deliberately does NOT require the keyboard to be online: an idle
    /// keyboard sleeps within minutes and probe timeouts can flap it offline,
    /// and tearing the capture session down on every nap would hand the
    /// diverted keys back to the firmware (dead bindings) until the re-arm
    /// races through. The session instead stays up across sleeps — its
    /// channel is to the always-present receiver — and re-arms diversion on
    /// the device's `0x1d4b` reconnection broadcast.
    fn keyboard_spec_for(&self) -> Option<KeyboardSpec> {
        let dev = self
            .devices
            .iter()
            .find(|d| d.kind == DeviceKind::Keyboard && d.route.is_some())?;
        let bindings = button_bindings_for(
            &self.config,
            Some(&dev.config_key),
            self.current_app.as_deref(),
        );
        let wanted: BTreeMap<u16, _> = KEYBOARD_KEY_CIDS
            .iter()
            .filter(|(_, button)| {
                bindings.get(button).is_some_and(|binding| {
                    matches!(binding, Binding::LongPress(_))
                        || binding.click_action() != Action::None
                })
            })
            .copied()
            .collect();
        if wanted.is_empty() {
            return None;
        }
        Some(KeyboardSpec {
            config_key: dev.config_key.clone(),
            route: dev.route.clone()?,
            wanted,
            bindings,
        })
    }

    /// Rewrite every shared map from the current config + selected device.
    fn rebuild(&self) {
        let key = self.current_key();
        self.publish_hook_maps(self.hook_maps_for(key, self.current_app.as_deref()));
        self.publish_device_runtime();
    }

    /// Republish the runtime views derived from the device set + config: the
    /// capture plans and the per-device DPI-cycle map. One method so the
    /// inventory fast path (same set, fresh online flags) can't update one and
    /// forget the other — a waking device needs both its capture session and
    /// its DPI-cycle slot.
    fn publish_device_runtime(&self) {
        self.publish_capture_plans();
        self.rebuild_dpi_cycles(self.current_key());
        // Keyboard F-key bindings are global (not per-device), so they key off
        // the top-level config map rather than the selected device. Published
        // here so `reload_config` (GUI commit) takes effect live, not only on
        // agent restart.
        write_value(
            &self.shared.keyboard_bindings,
            self.config.keyboard.bindings.clone(),
            "keyboard_bindings",
        );
        publish_arc_if_changed(
            &self.host_switch_links_tx,
            host_switch_links(&self.config, &self.devices),
        );
        publish_optional_arc_if_changed(&self.keyboard_spec_tx, self.keyboard_spec_for());
    }

    fn publish_capture_plans(&self) {
        publish_arc_if_changed(&self.capture_plans_tx, self.capture_plans_for());
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
        let rearm_generation = self.shared.capture_rearm_generation.load(Ordering::Relaxed);
        self.devices
            .iter()
            .filter(|dev| dev.online && self.config.device_enabled(&dev.config_key))
            .filter_map(|dev| {
                let route = dev.route.clone()?;
                let identity = DeviceIdentity::from_parts(dev.serial.as_deref(), dev.unit_id);
                let physical_key = canonical_device_key(&stable_id(dev), Some(&identity))
                    .or_else(|| PhysicalDeviceKey::parse(&dev.config_key))?;
                Some(plan_for_device(
                    &self.config,
                    DeviceCaptureIdentity {
                        physical_key,
                        config_key: &dev.config_key,
                        model_key: &dev.model_key,
                        route,
                    },
                    self.current_app.as_deref(),
                    rearm_generation,
                    self.os_mouse_hook_available,
                ))
            })
            .collect()
    }

    /// Publish whether the OS movement hook is currently usable.
    ///
    /// HID++ Back/Forward diversion follows this state as a fail-open policy:
    /// if the mouse-remapping hook is unavailable, side buttons remain native.
    /// Other HID++-only controls remain captured independently.
    pub fn set_os_mouse_hook_available(&mut self, available: bool) {
        if self.os_mouse_hook_available == available {
            return;
        }
        self.os_mouse_hook_available = available;
        self.publish_capture_plans();
    }

    /// Apply a fresh inventory snapshot. Always refreshes the snapshot the IPC
    /// `inventory()` poll serves (battery / online state changes without
    /// altering the device *set*), but only re-picks the selection and rebuilds
    /// the shared maps when the device set or runtime selection changed —
    /// `rebuild()` is driven by `config_key` + route and resets the live
    /// DPI-cycle index, so running it on every steady reconciliation
    /// would snap DPI back to `preset[0]` (and burn three `RwLock` writes)
    /// for nothing.
    pub fn refresh_inventory(
        &mut self,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        hid_open_failures: bool,
    ) {
        // Even an empty snapshot is a *completed* enumeration — the watcher
        // skips failed ticks — so the device set is now known either way (and
        // a recovered backend upgrades `Unavailable` back to live data).
        self.hid_open_failures = hid_open_failures;
        self.inventory = InventoryState::Ready {
            inventories: inventories.to_vec(),
            standalone: standalone.to_vec(),
        };
        self.publish_inventory();
        let devices = build_devices(&self.config, inventories, standalone);
        // Volatile settings (lighting colour, sensor DPI, SmartShift, native
        // wheel mode) live in device RAM and reset on a power cycle. Every
        // reconnect shape re-applies the persisted values (#189): a first
        // sighting, a replug (new route), a wake from device sleep
        // (offline→online), or — via the
        // flag — a system wake where none of those are observable.
        let reapply_all = std::mem::take(&mut self.reapply_all_next_refresh);
        let next_current = pick_current(&devices, self.config.selected_device());
        let rearm_capture = any_device_needs_capture_rearm(&self.devices, &devices, reapply_all);
        let followup = std::mem::take(&mut self.reapply_followup);
        let (targets, next_followup) =
            plan_reapply(&self.devices, &devices, &followup, reapply_all);
        self.reapply_followup = next_followup;
        for idx in targets {
            self.reapply_volatile_settings(&devices[idx]);
        }
        let changed = next_current != self.current
            || devices.len() != self.devices.len()
            || devices.iter().zip(&self.devices).any(|(a, b)| {
                a.config_key != b.config_key
                    || a.route != b.route
                    || a.capabilities != b.capabilities
                    || a.light_capabilities != b.light_capabilities
            });
        if rearm_capture {
            let generation = self
                .shared
                .capture_rearm_generation
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            debug!(generation, "device(s) require capture re-arm");
        }
        if !changed {
            // Same set, routes, and runtime selection — but keep the fresh
            // `online` flags, or a device that woke this tick would read as a
            // transition forever. The runtime views key on `online`, so
            // republish them even here or a woken device would get neither its
            // capture session nor its DPI-cycle slot. A rearm generation bump
            // also lands through this republish.
            self.devices = devices;
            self.publish_device_runtime();
            return;
        }
        self.devices = devices;
        self.current = next_current;
        self.rebuild();
    }

    /// Whether volatile-setting writes still need a delayed inventory pass to
    /// confirm them after device boot or system resume.
    #[must_use]
    pub fn needs_reapply_confirmation(&self) -> bool {
        !self.reapply_followup.is_empty()
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
    /// native wheel mode) to one device. Mouse settings run on one background
    /// thread and one HID++ channel so concurrent multi-open of the same
    /// receiver cannot cross-talk (#485); lighting stays a separate path
    /// (keyboards / different feature).
    fn reapply_volatile_settings(&self, dev: &AgentDevice) {
        // A disabled device is left fully native — no writes of any kind.
        if !self.config.device_enabled(&dev.config_key) {
            return;
        }
        let Some(route) = dev.route.clone() else {
            return;
        };
        let key = &dev.config_key;
        let route_key = stable_id(dev).route_key();
        let device = self.config.devices.get(key.as_str());
        let (resolution, inverted) = configured_wheel_mode(&self.config, dev);
        let dpi = device.and_then(|d| d.effective_dpi(&route_key));
        let smartshift = device
            .and_then(|d| d.effective_smartshift(&route_key))
            .map(openlogi_hid::SmartShiftStatus::from);
        if resolution.is_some() || inverted.is_some() || dpi.is_some() || smartshift.is_some() {
            crate::hardware::reapply_mouse_volatile_in_background(
                &self.shared.device(&route),
                resolution,
                inverted,
                dpi,
                smartshift,
            );
        }
        if let Some(lighting) = device
            .and_then(|d| d.effective_lighting(&route_key))
            .filter(|l| l.enabled)
        {
            crate::hardware::set_lighting_in_background(self.shared.device(&route), lighting);
        }
        if let Some(fn_lock) = self.config.fn_lock(key) {
            crate::hardware::write_fn_lock_in_background(
                self.shared.keyboard_device(&route),
                fn_lock,
            );
        }
        if let Some(capabilities) = dev.light_capabilities
            && let Some(light) = self.effective_light_settings(key)
        {
            crate::hardware::set_light_in_background(
                &self.shared.device_io,
                Some(route),
                &light,
                capabilities,
            );
        }
    }

    /// Apply an aggregate camera-use transition to every opted-in online
    /// light. Only effective power is transient; persisted manual power and
    /// the remaining light settings are unchanged.
    pub fn set_camera_active(&mut self, active: bool) {
        if self.camera_active == Some(active) {
            return;
        }
        let previous = self.camera_active;
        self.camera_active = Some(active);
        self.observable.set_camera_active(active);
        self.manual_light_overrides.clear();
        let mut applied = 0;
        for dev in self
            .devices
            .iter()
            .filter(|dev| dev.online && dev.route.is_some())
        {
            let (Some(capabilities), Some(mut light)) = (
                dev.light_capabilities,
                self.config
                    .light(&dev.config_key)
                    .filter(|light| light.auto_camera),
            ) else {
                continue;
            };
            light.enabled = active;
            crate::hardware::set_light_in_background(
                &self.shared.device_io,
                dev.route.clone(),
                &light,
                capabilities,
            );
            applied += 1;
        }
        info!(previous = ?previous, active, lights = applied, "applied camera-linked light state");
    }

    /// Resolve settings for reconnect/config re-application. Camera policy and
    /// a transient manual override replace only the effective power field.
    fn effective_light_settings(&self, key: &str) -> Option<LightSettings> {
        let mut light = self.config.light(key)?;
        if light.auto_camera {
            if let Some(override_enabled) = self.manual_light_overrides.get(key) {
                light.enabled = *override_enabled;
            } else if let Some(active) = self.camera_active {
                light.enabled = active;
            }
        }
        Some(light)
    }

    /// Store a transient manual power choice for a known light route. The IPC
    /// write can race the config reload that first enabled camera automation,
    /// so route/capability identity—not the possibly-stale config bit—is the
    /// acceptance condition. A reload retains it only while the new config is
    /// camera-linked.
    pub fn set_manual_light_power(&mut self, route: &DeviceRoute, enabled: bool) -> bool {
        let Some(device) = self.devices.iter().find(|device| {
            device.route.as_ref() == Some(route) && device.light_capabilities.is_some()
        }) else {
            return false;
        };
        self.manual_light_overrides
            .insert(device.config_key.clone(), enabled);
        true
    }

    /// Push the saved native wheel resolution/inversion to every currently online
    /// device. Separated from [`Self::rebuild`] (which also runs on
    /// foreground-app changes) because the HID++ write is only needed when
    /// config or device presence changes. The write short-circuits at the
    /// `0x2121` layer when the wheel already holds the desired state, so calling
    /// it on every reload costs at most one wheel-mode read per device — and
    /// still recovers a device whose earlier write timed out while it was waking.
    fn apply_native_wheel_modes(&self) {
        for dev in self.devices.iter().filter(|dev| dev.online) {
            let Some(route) = dev.route.clone() else {
                continue;
            };
            let (resolution, inverted) = configured_wheel_mode(&self.config, dev);
            crate::hardware::write_scroll_wheel_mode_in_background(
                self.shared.device(&route),
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
            InventoryState::Ready { inventories, .. } => inventories.clone(),
            InventoryState::Pending | InventoryState::Unavailable => Vec::new(),
        }
    }

    /// The latest standalone raw-HID inventory snapshot.
    #[must_use]
    pub fn standalone(&self) -> Vec<StandaloneDevice> {
        match &self.inventory {
            InventoryState::Ready { standalone, .. } => standalone.clone(),
            InventoryState::Pending | InventoryState::Unavailable => Vec::new(),
        }
    }

    /// Snapshot the active device and effective ring layout for a new
    /// invocation. `None` means the ring is disabled, empty, or has no active
    /// persistent device. The returned layout is owned so later config and
    /// foreground-app changes cannot alter an already-open ring session.
    #[must_use]
    pub fn action_ring_session(
        &self,
        triggering_device: Option<&str>,
    ) -> Option<ActionRingSessionSpec> {
        let key = triggering_device.or_else(|| self.current_key())?;
        let device = self
            .devices
            .iter()
            .find(|device| device.config_key == key)?;
        let ring = self.config.action_ring(key);
        if !ring.enabled {
            return None;
        }
        let layout = ring.effective_layout(self.current_app.as_deref());
        if layout.slots.is_empty() {
            return None;
        }
        let haptic_route = (device.online
            && ring.haptics
            && device
                .capabilities
                .is_some_and(|capabilities| capabilities.haptic_feedback))
        .then(|| device.route.clone())
        .flatten();
        Some(ActionRingSessionSpec {
            device_key: key.to_owned(),
            haptic_route,
            layout,
            language: self.config.app_settings.language.clone(),
        })
    }

    /// Where enumeration stands, for the IPC `status` poll.
    #[must_use]
    pub fn inventory_health(&self) -> InventoryHealth {
        match self.inventory {
            InventoryState::Pending => InventoryHealth::Scanning,
            InventoryState::Ready { .. } => InventoryHealth::Ready,
            InventoryState::Unavailable => InventoryHealth::Unavailable,
        }
    }

    /// Republish the device facts the GUI observes, reading the one field that
    /// holds them so this can never disagree with the `inventory` /
    /// `standalone` / `inventory_health` accessors. Called by every mutator
    /// that touches the `inventory` field, and it clones only when something
    /// actually changed.
    fn publish_inventory(&self) {
        let health = self.inventory_health();
        match &self.inventory {
            InventoryState::Ready {
                inventories,
                standalone,
            } => {
                self.observable.set_inventory(
                    health,
                    inventories,
                    standalone,
                    self.hid_open_failures,
                );
            }
            InventoryState::Pending | InventoryState::Unavailable => {
                self.observable
                    .set_inventory(health, &[], &[], self.hid_open_failures);
            }
        }
    }

    /// Record that enumeration has never worked and has stopped being treated
    /// as "still starting" (persistent initial failure, or the watcher died).
    /// Downgrades only the pending state: once a snapshot exists the
    /// last good device set stays authoritative — the same policy as the
    /// watcher skipping failed mid-session ticks.
    pub fn mark_inventory_unavailable(&mut self) {
        if matches!(self.inventory, InventoryState::Pending) {
            self.inventory = InventoryState::Unavailable;
            self.publish_inventory();
        }
    }

    /// Foreground-app change → re-overlay per-app bindings on the hook maps and
    /// republish the capture plans, whose binding maps and divert sets are
    /// per-app effective too (HID++ dispatch reads them at event time). Both
    /// hook maps are recomputed: a per-app override of the gesture owner turns
    /// it into a single action for that app, dropping it from the OS-hook
    /// gesture set — so the gesture map is app-scoped too. The dedicated HID++
    /// gesture map is not app-scoped and stays untouched.
    ///
    /// Only the identifier decides whether any of that runs: an application
    /// that merely changed its localized name resolves to the same bindings,
    /// and republishing for it could restart a capture session (a plan's
    /// divert set is part of its identity) over nothing. The observable cell
    /// still gets the whole value — it dedupes on its own, and its recent list
    /// is the only source a client has for these identifiers. Returns whether
    /// the effective app identifier changed and active button lifecycles must
    /// be canceled.
    pub fn set_current_app(&mut self, app: Option<ForegroundApp>) -> bool {
        let id = app.as_ref().map(|app| app.id.clone());
        self.observable.set_foreground(app);
        if id == self.current_app {
            return false;
        }
        self.current_app = id;
        self.publish_hook_maps(self.hook_maps_for(self.current_key(), self.current_app.as_deref()));
        // Capture plans are app-scoped (per-app binding overlays); republish
        // them with the keyboard's effective bindings.
        self.publish_device_runtime();
        true
    }

    /// Replace the config (after `config.toml` changed) and rebuild everything.
    pub fn reload_config(&mut self, config: Config) {
        // Parameter-only edits must not erase a transient manual choice while
        // the light remains camera-linked. Changing the policy invalidates it.
        self.config = config;
        self.shared.scroll_preferences.publish(
            self.config.app_settings.smooth_scroll,
            self.config.app_settings.vertical_scroll_sensitivity,
        );
        self.observable
            .set_launch_at_login(self.config.app_settings.launch_at_login);
        let retained_overrides: HashSet<String> = self
            .manual_light_overrides
            .keys()
            .filter(|key| {
                self.config
                    .light(key)
                    .is_some_and(|light| light.auto_camera)
            })
            .cloned()
            .collect();
        self.manual_light_overrides
            .retain(|key, _| retained_overrides.contains(key));
        self.current = pick_current(&self.devices, self.config.selected_device());
        self.rebuild();
        self.apply_native_wheel_modes();
        self.apply_fn_locks();
        self.reapply_light_settings();
    }

    /// Push the saved Fn-lock state to every online keyboard that has one.
    /// Runs on config reloads (the reconnect path is
    /// [`Self::reapply_volatile_settings`]); the write is a single HID++ call,
    /// so re-applying an unchanged state is cheap.
    fn apply_fn_locks(&self) {
        for dev in self.devices.iter().filter(|dev| dev.online) {
            let Some(route) = dev.route.clone() else {
                continue;
            };
            if let Some(fn_lock) = self.config.fn_lock(&dev.config_key) {
                crate::hardware::write_fn_lock_in_background(
                    self.shared.keyboard_device(&route),
                    fn_lock,
                );
            }
        }
    }

    /// Re-apply standalone-light settings after a config reload.
    fn reapply_light_settings(&self) {
        for dev in self
            .devices
            .iter()
            .filter(|dev| dev.online && dev.route.is_some() && dev.light_capabilities.is_some())
        {
            if let (Some(light), Some(capabilities)) = (
                self.effective_light_settings(&dev.config_key),
                dev.light_capabilities,
            ) {
                crate::hardware::set_light_in_background(
                    &self.shared.device_io,
                    dev.route.clone(),
                    &light,
                    capabilities,
                );
            }
        }
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
    let route_key = stable_id(dev).route_key();
    let device = config.devices.get(dev.config_key.as_str());
    let resolution = capabilities
        .hires_wheel
        .then(|| device.and_then(|d| d.effective_scroll_resolution(&route_key)))
        .flatten();
    let inverted = capabilities
        .scroll_inversion
        .then(|| device.is_some_and(|d| d.effective_invert_scroll(&route_key)));
    (resolution, inverted)
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
fn build_devices(
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
            let route = DeviceRoute::device_route_for(inv, paired.slot);
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
                model_key: model.config_key(),
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
        let route = DeviceRoute::RawHid {
            vendor_id: device.address.vendor_id,
            product_id: device.address.product_id,
            usage_page: device.address.usage_page,
            usage_id: device.address.usage_id,
            identity: device.address.identity.clone(),
        };
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

fn host_switch_links(config: &Config, devices: &[AgentDevice]) -> Vec<HostSwitchLink> {
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

/// Whether this refresh invalidated any online device's volatile control
/// diversion. Receiver routes stay connected while a paired mouse sleeps, so
/// route equality alone cannot tell capture sessions to re-arm on wake.
fn any_device_needs_capture_rearm(
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
const VOLATILE_REAPPLY_CONFIRM_RETRIES: u8 = 4;

/// Plan this refresh's volatile-settings writes: the [`reapply_targets`] set
/// plus a bounded run of confirming re-applies for devices first sighted
/// recently or targeted by a system wake, and the follow-up keys (with
/// remaining retry counts) to confirm next refresh. Reconnects
/// (offline→online) re-apply once — the device was already booted, so it
/// needs no boot-race retry.
fn plan_reapply(
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
fn pick_current(devices: &[AgentDevice], saved: Option<&str>) -> usize {
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

fn is_hidpp_device(device: &AgentDevice) -> bool {
    !matches!(device.route, Some(DeviceRoute::RawHid { .. }))
}

/// Replace the value behind an `RwLock`, logging (not panicking) on poison so a
/// background thread that panicked while holding the lock can't take the agent
/// down — it just keeps the stale value until the next successful rebuild.
fn write_value<T>(lock: &RwLock<T>, value: T, name: &str) {
    match lock.write() {
        Ok(mut guard) => *guard = value,
        Err(e) => warn!(error = %e, lock = name, "lock poisoned — keeping stale value"),
    }
}

/// Publish a fresh immutable snapshot only when its projected value changed.
fn publish_arc_if_changed<T: PartialEq>(publication: &watch::Sender<Arc<T>>, value: T) {
    publication.send_if_modified(|current| {
        if current.as_ref() == &value {
            return false;
        }
        *current = Arc::new(value);
        true
    });
}

fn publish_optional_arc_if_changed<T: PartialEq>(
    publication: &watch::Sender<Option<Arc<T>>>,
    value: Option<T>,
) {
    publication.send_if_modified(|current| {
        if current.as_deref() == value.as_ref() {
            return false;
        }
        *current = value.map(Arc::new);
        true
    });
}

#[cfg(test)]
mod tests;
