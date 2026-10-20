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
use openlogi_core::binding::{Binding, ButtonId};
use openlogi_core::bindings::{button_bindings_for, oshook_gestures_for};
use openlogi_core::config::{
    Config, ConfigRevision, LightSettings, MouseProfileTarget, canonical_device_key,
};
use openlogi_core::device::{
    BatteryInfo, Capabilities, ChangedOnLight, DeviceInventory, DeviceKind, LightCapabilities,
    LightState, StandaloneDevice, prettify_codename,
};
use openlogi_core::device_order::{DeviceIdentity, PhysicalDeviceKey};
use openlogi_hid::{
    BacklightChange, BacklightState, CaptureChannelSlot, ChannelPool, ChannelRegistry,
    DeviceIoGate, DeviceRoute, EasySwitchHosts, FnLockState, HidppOperation, HostOs, ReportRate,
    ReportRateInfo, WriteError, commands_for_light_settings, is_reserved_keyboard_control,
};
#[cfg(test)]
use openlogi_ipc::ChosenLight;
use openlogi_ipc::{InventoryHealth, LockKeyChange, NoticeKind, SavedConfig};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::action_ring::ActionRingSessionSpec;
use crate::capture_plan::{
    CapturePublication, SharedCapturePlans, hidpp_side_gesture_maps_for, plan_for_device,
};
use crate::hardware::{
    DeviceAccess, DeviceOp, HardwareContext, KeptOsLayouts, VolatileMouseSettings, WriteOrder,
};
use crate::notices::LowBatteryWatch;
use crate::observable::ObservableState;
use crate::receiver_access::ReceiverAccess;
use crate::runtime::hook::{HookMaps, SharedHookMaps};
use crate::runtime::scroll::ScrollPreferences;
use crate::watchers::host_switch::{HostSwitchLink, HostSwitchLinks};
use crate::watchers::inventory::InventoryRefresh;
use crate::watchers::keyboard::{KeyboardSpec, SharedKeyboardSpec};
use crate::watchers::light::{WatchedLight, WatchedLights};
use crate::{DpiCycleState, DpiCycles};

mod devices;
mod lights;

pub use lights::{ConfigSource, LightWriteClaim, TakenLightChange, write_light};

#[cfg(test)]
use devices::reapply_targets;
use devices::{
    RECONNECT_REAPPLY_CONFIRM_RETRIES, VOLATILE_REAPPLY_CONFIRM_RETRIES,
    any_device_needs_capture_rearm, build_devices, config_keeps_os_layout, configured_report_rate,
    configured_wheel_mode, host_switch_links, is_hidpp_device, keeps_os_layout, main_wheel_divert,
    newly_kept_os_layouts, os_layout_appeared, pick_current, plan_reapply, settings_moved,
    stable_id, wheel_inverted,
};

/// The minimal per-device facts the agent needs: the config key (binding /
/// preset lookup), the HID++ route (DPI/SmartShift writes + capture target), and
/// the identity fields the canonical ordering keys on (so the no-selection
/// fallback agrees with the GUI carousel — see [`openlogi_core::device_order`]).
struct AgentDevice {
    config_key: String,
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
    /// [`PairedDevice::settings_revision`](openlogi_core::device::PairedDevice::settings_revision):
    /// it moves when the device reports a setting changed on it. Comparable
    /// only between snapshots of the same route.
    settings_revision: u64,
    /// The name the device reports over HID++, for its low-battery notice.
    codename: Option<String>,
    /// The device's latest battery reading, `None` when it has none.
    battery: Option<BatteryInfo>,
}

/// Cheaply cloneable handles handed to hooks and background managers.
/// The orchestrator remains the sole producer for its watch-backed projections;
/// consumers receive only read capabilities through this type.
#[derive(Clone)]
pub struct SharedHandles {
    /// Backend identity, I/O gate, channel pool, and inventory source shared by
    /// every hardware-dependent agent service.
    hardware: HardwareContext,
    /// The OS-hook callback's single-action + gesture maps, behind one lock so a
    /// rebuild publishes both atomically (see [`HookMaps`]). Also read by the
    /// gesture watcher for thumb-wheel input and DPI-button actions/gestures.
    pub hook_maps: SharedHookMaps,
    /// Function-key remapper bindings (keycode+modifiers → action). Not
    /// per-app-profile in M1 (spec non-goal), so a single shared map.
    pub keyboard_bindings: crate::runtime::hook::SharedKeyboardBindings,
    /// Live smooth-scroll and vertical sensitivity settings read without
    /// taking the orchestrator/config lock.
    pub scroll_preferences: Arc<ScrollPreferences>,
    pub dpi_cycle: Arc<RwLock<DpiCycles>>,
    /// Capture plans for online enabled devices, plus the latest route of
    /// every online physical device. A disabled device stays out of the plans
    /// so capture leaves it native, but its route remains for restoration.
    pub capture_plans: SharedCapturePlans,
    pub capture_channel: CaptureChannelSlot,
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
    pub keyboard_channel: CaptureChannelSlot,
    /// Incremented when a device reconnects or the system wakes, so capture
    /// sessions re-arm volatile HID++ control diversion even when route and
    /// online flags look unchanged.
    pub capture_rearm_generation: Arc<AtomicU64>,
    /// Receiver access shared by HID++ sessions and pairing. Pairing/host
    /// transitions are exclusive; capture sessions share under read leases.
    pub receiver_access: ReceiverAccess,
    /// Per-transport turns for agent-owned HID requests. Shared by every
    /// device access built from these handles, so two slots of one receiver
    /// queue behind each other before either starts its response timeout.
    transport_turns: crate::hardware::TransportTurns,
    /// Keyboard → pointing-device routes resolved from `config.toml`.
    pub host_switch_links: HostSwitchLinks,
    /// The online lights to watch for changes made on the light itself.
    pub watched_lights: WatchedLights,
    /// Orders every path's Fn-lock writes per keyboard.
    fn_lock_order: WriteOrder,
    /// Orders the GUI's report-rate writes and the reconnect reapply's per
    /// mouse.
    report_rate_order: WriteOrder,
    /// Orders the GUI's backlight writes per keyboard.
    backlight_order: WriteOrder,
    /// Orders host renames per device.
    host_name_order: WriteOrder,
    /// The running inventory watcher's refresh handle, published at arming;
    /// `None` while no watcher runs.
    inventory_refresh: Arc<RwLock<Option<InventoryRefresh>>>,
}

impl SharedHandles {
    /// The hardware context these handles were built on.
    #[must_use]
    pub fn hardware(&self) -> HardwareContext {
        self.hardware.clone()
    }

    /// Device access through the mouse/pointer capture channel — the
    /// registry-confirmed capture channel or the exact current inventory
    /// channel that every device write already resolves through.
    #[must_use]
    pub fn device_access(&self) -> DeviceAccess {
        self.access_through(&self.capture_channel)
    }

    /// Same, but through the keyboard capture channel — Fn-lock writes run on
    /// the keyboard's own capture session, not the mouse-oriented
    /// [`Self::capture_channel`].
    #[must_use]
    pub fn keyboard_access(&self) -> DeviceAccess {
        self.access_through(&self.keyboard_channel)
    }

    fn access_through(&self, channel: &CaptureChannelSlot) -> DeviceAccess {
        DeviceAccess {
            channel: channel.clone(),
            registry: self.channel_registry.clone(),
            receiver_access: self.receiver_access.clone(),
            device_io: self.device_io.clone(),
            transport_turns: self.transport_turns.clone(),
        }
    }

    /// Bind a device operation to `route` through [`Self::device_access`].
    #[must_use]
    pub fn device(&self, route: &DeviceRoute) -> DeviceOp {
        self.device_access().op(route)
    }

    /// Bind a device operation to `route` through [`Self::keyboard_access`].
    #[must_use]
    pub fn keyboard_device(&self, route: &DeviceRoute) -> DeviceOp {
        self.keyboard_access().op(route)
    }

    /// Return the `0x8071` software control this session's lighting writes
    /// took to the firmware. The agent calls this once on a terminal exit,
    /// while inventory still publishes its channels. A handover or replacement
    /// skips it: the successor repaints on arrival, and its own exit hands
    /// control back.
    pub async fn release_rgb_control(&self) {
        crate::hardware::release_session_rgb_control(&self.device_access()).await;
    }

    /// Write `fn_lock` to the keyboard at `route` and return its echo. When a
    /// newer Fn-lock write for the keyboard is requested before this one gets
    /// its turn, this one reads the keyboard instead of writing a value that
    /// is about to be replaced. Waiting for the turn does not count against
    /// the write's deadline.
    pub async fn set_fn_lock(
        &self,
        route: &DeviceRoute,
        fn_lock: bool,
    ) -> Result<FnLockState, WriteError> {
        let ticket = self.fn_lock_order.request(route);
        self.keyboard_device(route)
            .run_in_turn(
                HidppOperation::WriteFnLock,
                &ticket,
                |c| async move { openlogi_hid::set_fn_lock_on(&c, fn_lock).await },
                |c| async move { openlogi_hid::get_fn_lock_on(&c).await },
            )
            .await
    }

    /// Write `rate` to the mouse at `route` and return what it reads back.
    /// When a newer report-rate write for the mouse is requested before this
    /// one gets its turn, this one reads the mouse instead of writing a rate
    /// that is about to be replaced. A reconnect reapply numbers its write
    /// when it is scheduled, so it never lands over a pick made after that,
    /// and a pick queued behind a reapply still gets the whole deadline once
    /// its turn comes.
    pub async fn set_report_rate(
        &self,
        route: &DeviceRoute,
        rate: ReportRate,
    ) -> Result<ReportRateInfo, WriteError> {
        let ticket = self.report_rate_order.request(route);
        self.device(route)
            .run_in_turn(
                HidppOperation::WriteReportRate,
                &ticket,
                |c| async move { openlogi_hid::set_report_rate_on(&c, rate).await },
                |c| async move { openlogi_hid::get_report_rate_info_on(&c).await },
            )
            .await
    }

    /// Apply `change` to the backlight of the keyboard at `route` and return
    /// what it reads back. Every change is applied, one at a time in request
    /// order: each rewrites the whole configuration it read, so two that
    /// overlapped would undo one another, and a newer change to another
    /// control does not make an older one moot. Waiting for the earlier
    /// changes does not count against the write's deadline.
    pub async fn set_backlight(
        &self,
        route: &DeviceRoute,
        change: BacklightChange,
    ) -> Result<BacklightState, WriteError> {
        let ticket = self.backlight_order.request(route);
        self.device(route)
            .run_in_order(
                HidppOperation::WriteBacklight,
                ticket,
                move |c| async move { openlogi_hid::update_backlight_on(&c, change).await },
            )
            .await
    }

    /// Name the computer on Easy-Switch `slot` of the device at `route` and
    /// return the slots it reads back. Renames run one at a time in request
    /// order, so the name asked for last is the one the device keeps.
    /// Waiting for the earlier renames does not count against the write's
    /// deadline.
    pub async fn rename_easy_switch_host(
        &self,
        route: &DeviceRoute,
        slot: u8,
        name: String,
    ) -> Result<EasySwitchHosts, WriteError> {
        let ticket = self.host_name_order.request(route);
        self.device(route)
            .run_in_order(HidppOperation::RenameHost, ticket, move |c| async move {
                openlogi_hid::rename_easy_switch_host_on(&c, slot, &name).await
            })
            .await
    }

    /// Hand requests to the inventory watcher started at arming.
    pub fn publish_inventory_refresh(&self, refresh: InventoryRefresh) {
        write_value(&self.inventory_refresh, Some(refresh), "inventory refresh");
    }

    /// Have the inventory watcher rescan receivers after a pairing-table
    /// change. Nothing is scanning while the agent is unarmed, and arming
    /// starts with a full scan, so there is then nothing to ask.
    pub fn request_receiver_rescan(&self) {
        let Ok(refresh) = self.inventory_refresh.read() else {
            warn!("inventory refresh handle poisoned — receiver rescan skipped");
            return;
        };
        if let Some(refresh) = refresh.as_ref() {
            refresh.request_receiver_rescan();
        }
    }

    /// [`Self::set_fn_lock`] without waiting, for the config-reload and
    /// reconnect paths; the outcome is logged.
    fn write_fn_lock_in_background(&self, route: &DeviceRoute, fn_lock: bool) {
        crate::hardware::write_fn_lock_in_background(
            self.keyboard_device(route),
            self.fn_lock_order.request(route),
            fn_lock,
        );
    }
}

/// Owns the config + device selection and keeps [`SharedHandles`] in sync.
pub struct Orchestrator {
    config: Config,
    devices: Vec<AgentDevice>,
    current: usize,
    current_app: Option<String>,
    pointer_context: openlogi_hook::PointerContext,
    /// The latest inventory snapshot, kept so the IPC server can answer the
    /// GUI's `inventory()` polls without re-enumerating (the agent owns all
    /// device I/O). The enum keeps "nothing checked yet" and "enumeration
    /// broken" distinct from "checked and empty" — the IPC `status` reports
    /// the distinction (as [`InventoryHealth`]) so the GUI can tell them
    /// apart.
    inventory: InventoryState,
    /// Set after a system wake or a device's own reconnect broadcast: devices
    /// may have power-cycled while their set/route/online state looks
    /// identical across the gap, so the next refresh re-applies volatile
    /// settings to every online device. The value is the confirming re-apply
    /// budget the forced targets are queued with — a system wake keeps the
    /// boot-race ladder, a device reconnect needs only a link-race confirm.
    forced_reapply_budget: Option<u8>,
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
    /// What each light reached over HID++ last read back, by config key,
    /// with the route it was read on.
    light_states: BTreeMap<String, (DeviceRoute, LightState)>,
    /// Settings changed on each light itself that the config does not hold
    /// yet, by config key. They stand in for the saved values until a save
    /// or a GUI write of that control replaces them.
    lights_changed_on_light: BTreeMap<String, ChangedOnLight>,
    /// The changes made on lights that GUI writes in flight took, by claim
    /// ([`LightWriteClaim`]). A reload replaces them as it replaces the
    /// changes above, and a failed write puts back what is left. A write
    /// whose RPC is cancelled before it settles leaves its claim here; it is
    /// never put back.
    light_writes: BTreeMap<u64, TakenLightChange>,
    /// The number of the next [`LightWriteClaim`].
    next_light_write: u64,
    /// The keyboards whose "keep OS layout" is in effect, shared with the
    /// layout writes queued for them so a write withdraws itself once a
    /// reload turns the setting off or disables the device.
    kept_os_layouts: Arc<KeptOsLayouts>,
    /// Whether the OS mouse hook is currently installed. Back/Forward gesture
    /// motion comes from HID++, but diversion is published only while the
    /// broader mouse-remapping path is available so losing the hook leaves the
    /// side buttons native.
    os_mouse_hook_available: bool,
    /// Private producer halves for the read-only runtime projections in
    /// `shared`, keeping the orchestrator's single-writer contract structural.
    capture_plans_tx: watch::Sender<Arc<CapturePublication>>,
    keyboard_spec_tx: watch::Sender<Option<Arc<KeyboardSpec>>>,
    host_switch_links_tx: watch::Sender<Arc<Vec<HostSwitchLink>>>,
    watched_lights_tx: watch::Sender<Arc<Vec<WatchedLight>>>,
    shared: SharedHandles,
    /// The state the GUI observes. Every mutator below that changes one of its
    /// facts republishes here, so the cell cannot go stale behind a new code
    /// path — see [`ObservableState`].
    observable: Arc<ObservableState>,
    /// Which devices' low batteries have been announced.
    low_battery: LowBatteryWatch,
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
    /// Build from a loaded config. Creates the shared handles and seeds
    /// them from the config with no devices yet; the first inventory tick fills
    /// in the routes and presets.
    ///
    /// `observable` is the cell the IPC server answers from; the config facts
    /// it carries are seeded here.
    #[must_use]
    pub fn new(config: Config, observable: Arc<ObservableState>) -> Self {
        Self::with_hardware(config, observable, HardwareContext::production())
    }

    /// Build with an explicit hardware context. This is the injection boundary
    /// for replay and alternate backends; every backend-dependent shared handle
    /// is derived from `hardware`.
    #[must_use]
    pub fn with_hardware(
        config: Config,
        observable: Arc<ObservableState>,
        hardware: HardwareContext,
    ) -> Self {
        let (capture_plans_tx, capture_plans) =
            watch::channel(Arc::new(CapturePublication::default()));
        let (keyboard_spec_tx, keyboard_spec) = watch::channel(None);
        let (host_switch_links_tx, host_switch_links) = watch::channel(Arc::new(Vec::new()));
        let (watched_lights_tx, watched_lights) = watch::channel(Arc::new(Vec::new()));
        let shared = SharedHandles {
            device_io: hardware.device_io(),
            channel_pool: hardware.channel_pool(),
            hardware,
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
            keyboard_spec,
            keyboard_channel: Arc::new(RwLock::new(None)),
            capture_rearm_generation: Arc::new(AtomicU64::new(0)),
            receiver_access: ReceiverAccess::default(),
            transport_turns: crate::hardware::TransportTurns::default(),
            host_switch_links,
            watched_lights,
            fn_lock_order: WriteOrder::default(),
            report_rate_order: WriteOrder::default(),
            backlight_order: WriteOrder::default(),
            host_name_order: WriteOrder::default(),
            inventory_refresh: Arc::new(RwLock::new(None)),
        };
        let orch = Self {
            config,
            devices: Vec::new(),
            current: 0,
            current_app: None,
            pointer_context: openlogi_hook::PointerContext {
                app: None,
                target: openlogi_hook::PointerTarget::Unavailable,
            },
            inventory: InventoryState::Pending,
            forced_reapply_budget: None,
            hid_open_failures: false,
            reapply_followup: HashMap::new(),
            camera_active: None,
            manual_light_overrides: BTreeMap::new(),
            light_states: BTreeMap::new(),
            lights_changed_on_light: BTreeMap::new(),
            light_writes: BTreeMap::new(),
            next_light_write: 0,
            kept_os_layouts: Arc::default(),
            os_mouse_hook_available: false,
            capture_plans_tx,
            keyboard_spec_tx,
            host_switch_links_tx,
            watched_lights_tx,
            shared,
            observable,
            low_battery: LowBatteryWatch::default(),
        };
        orch.rebuild();
        orch.observable
            .set_launch_at_login(orch.config.app_settings.launch_at_login);
        orch
    }

    /// A cheap clone of the shared `Arc`s to hand to the watchers and hook.
    #[must_use]
    pub fn shared(&self) -> SharedHandles {
        self.shared.clone()
    }

    fn current_key(&self) -> Option<&str> {
        self.devices
            .get(self.current)
            .filter(|device| is_hidpp_device(device))
            .map(|d| d.config_key.as_str())
    }

    /// The app whose mouse profile applies, and the pointer target dispatch
    /// revalidates (`None`: dispatch follows focus, unrevalidated).
    ///
    /// An unidentified target (an overlay, a failed lookup) can last a whole
    /// session. It selects the focused profile, never the desktop's, yet stays
    /// pointer-scoped: its presses still end when the pointer reaches an
    /// identified target, whose profile they were not resolved against. With
    /// no focused application there is no such profile and the global bindings
    /// apply, as focused mode applies them in the same state.
    fn mouse_context(&self) -> (Option<&str>, Option<openlogi_hook::PointerTarget>) {
        let target = self.pointer_context.target;
        if self.config.app_settings.mouse_profile_target == MouseProfileTarget::Focused
            || target == openlogi_hook::PointerTarget::Unsupported
        {
            return (self.current_app.as_deref(), None);
        }
        let app = if target == openlogi_hook::PointerTarget::Unavailable {
            self.current_app.as_deref()
        } else {
            self.pointer_context.app.as_ref().map(|app| app.id.as_str())
        };
        (app, Some(target))
    }

    /// Build the OS-hook callback's maps for `key` and its mouse context. Both hook
    /// sub-maps are app-scoped (a per-app override can demote the gesture owner),
    /// so they're built together here and published under one lock — keeping
    /// `rebuild` and `set_current_app` from drifting into a half-populated write.
    fn hook_maps_for(&self, key: Option<&str>) -> HookMaps {
        // A disabled selected device gets empty maps: the OS hook then passes
        // its events through untouched instead of applying remaps to a device
        // the user asked OpenLogi to leave alone.
        if key.is_some_and(|k| !self.config.device_enabled(k)) {
            return HookMaps::default();
        }
        let (app, pointer_target) = self.mouse_context();
        let mut bindings = button_bindings_for(&self.config, key, app);
        let mut gestures = oshook_gestures_for(&self.config, key, app);
        if cfg!(target_os = "macos")
            && let Some(key) = key
        {
            for button in hidpp_side_gesture_maps_for(&self.config, key, app)
                .keys()
                .filter(|&&button| button != ButtonId::MiddleClick)
            {
                // macOS gives HID++ exclusive ownership of both edges of
                // Back/Forward. Windows, and macOS for the wheel button whose
                // raw-XY support varies by device, keep this global map as a
                // passive fallback when the physical control cannot arm raw XY.
                bindings.remove(button);
                gestures.remove(button);
            }
        }
        HookMaps {
            bindings,
            gestures,
            pointer_target,
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

    /// The keyboard key-capture spec for the managed keyboard, or `None` when
    /// no enabled keyboard carries a real binding (an unbound key must never
    /// be diverted).
    ///
    /// The bound keys *are* the divert set: every
    /// [`ButtonId::Control`] in the
    /// keyboard's effective bindings names the `0x1b04` control to divert, and
    /// the capture session arms whichever of those the device's own control
    /// table reports as divertable. No fixed key table sits in between, so a
    /// keyboard OpenLogi has never seen is remappable the day it ships.
    ///
    /// One session at a time, and only a keyboard with something bound can
    /// hold it: an online bound keyboard wins over an asleep one, so a stale
    /// receiver slot for the same model (#1581) — which carries no bindings —
    /// never takes the session from the keyboard that is actually typing, and
    /// never shadows it while it naps. Among bound keyboards in the same
    /// online state, inventory order breaks the tie. Beyond that the spec
    /// deliberately does NOT require the keyboard to be online: an idle
    /// keyboard sleeps within minutes and probe timeouts can flap it offline,
    /// and tearing the capture session down on every nap would hand the
    /// diverted keys back to the firmware (dead bindings) until the re-arm
    /// races through. The session instead stays up across sleeps — its
    /// channel is to the always-present receiver — and re-arms diversion on
    /// the device's `0x1d4b` reconnection broadcast.
    fn keyboard_spec_for(&self) -> Option<KeyboardSpec> {
        let candidates: Vec<KeyboardCandidate<'_>> = self
            .devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Keyboard && d.route.is_some())
            .filter(|d| self.config.device_enabled(&d.config_key))
            .filter_map(|dev| {
                let bindings = button_bindings_for(
                    &self.config,
                    Some(&dev.config_key),
                    self.current_app.as_deref(),
                );
                let divert = keyboard_divert_set(&bindings);
                (!divert.wanted.is_empty()).then_some(KeyboardCandidate {
                    dev,
                    bindings,
                    divert,
                })
            })
            .collect();
        let chosen = candidates
            .iter()
            .find(|candidate| candidate.dev.online)
            .or_else(|| candidates.first())?;
        for &cid in &chosen.divert.reserved {
            warn!(
                cid = format_args!("{cid:#06x}"),
                key = %chosen.dev.config_key,
                "binding names a control OpenLogi never diverts as a key — left native"
            );
        }
        Some(KeyboardSpec {
            config_key: chosen.dev.config_key.clone(),
            route: chosen.dev.route.clone()?,
            wanted: chosen.divert.wanted.clone(),
            bindings: chosen.bindings.clone(),
        })
    }

    /// Rewrite every shared map from the current config + selected device.
    fn rebuild(&self) {
        let key = self.current_key();
        self.publish_hook_maps(self.hook_maps_for(key));
        self.publish_scroll_inversion(key);
        self.publish_device_runtime();
    }

    /// Tell the capture layer whether to reverse the wheel for the selected
    /// device.
    ///
    /// Only for devices with no native `0x2121` invert bit: one that has it
    /// gets the firmware write from [`configured_wheel_mode`] instead, and
    /// doing both would cancel out. A device that is disabled, unselected or
    /// not asking for inversion turns the flag off, so the hook stops
    /// transforming as soon as the setting is withdrawn.
    fn publish_scroll_inversion(&self, key: Option<&str>) {
        let wanted = key.is_some_and(|key| {
            self.config.device_enabled(key)
                && self.config.invert_scroll(key)
                && self
                    .devices
                    .iter()
                    .find(|device| device.config_key == key)
                    .is_some_and(|device| {
                        device
                            .capabilities
                            .is_some_and(|capabilities| !capabilities.scroll_inversion)
                    })
        });
        openlogi_hook::set_scroll_inversion(wanted);
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
        publish_arc_if_changed(&self.watched_lights_tx, self.watched_lights());
        self.publish_lights();
    }

    fn publish_capture_plans(&self) {
        publish_arc_if_changed(&self.capture_plans_tx, self.capture_publication());
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

    /// Capture plans for enabled devices, and a route for every online device.
    ///
    /// A `0x1b04` control has one owner per device. The keyboard session owns
    /// every control the keyboard spec names, so those are released from the
    /// keyboard's own plan here: a hand-edited `Back = …` on a K380 would
    /// otherwise have both sessions divert the multiplatform Back key
    /// (`0x00bd`, a [`BACK_CIDS`](openlogi_hid::reprog_controls::BACK_CIDS)
    /// member), each reading the other's diverted state back as "original".
    ///
    /// Disabled devices are omitted from the plans so capture leaves them
    /// native. Their route is still published: a restore owed from an earlier
    /// handoff has to follow the mouse after capture stops.
    fn capture_publication(&self) -> CapturePublication {
        let rearm_generation = self.shared.capture_rearm_generation.load(Ordering::Relaxed);
        let keyboard = self.keyboard_spec_for();
        let mut plans = Vec::new();
        let mut routes = BTreeMap::new();
        for dev in &self.devices {
            let Some((physical_key, route)) = Self::capture_identity(dev) else {
                continue;
            };
            routes
                .entry(physical_key.clone())
                .or_insert_with(|| route.clone());
            if !self.config.device_enabled(&dev.config_key) {
                continue;
            }
            let (app, pointer_target) = if dev.kind == DeviceKind::Keyboard {
                (self.current_app.as_deref(), None)
            } else {
                self.mouse_context()
            };
            let mut plan = plan_for_device(
                &self.config,
                physical_key,
                &dev.config_key,
                route,
                app,
                rearm_generation,
                self.os_mouse_hook_available,
            );
            plan.dispatch.pointer_target = pointer_target;
            plan.dispatch.invert_wheel = wheel_inverted(&self.config, dev);
            plan.target.spec.divert_main_wheel =
                main_wheel_divert(&self.config, dev, self.os_mouse_hook_available);
            if let Some(keyboard) = keyboard
                .as_ref()
                .filter(|keyboard| keyboard.route == plan.target.route)
            {
                plan.release_controls(&keyboard.wanted);
            }
            plans.push(plan);
        }
        CapturePublication::new(plans, routes)
    }

    /// Physical key and route of one online device inventory can still address.
    fn capture_identity(dev: &AgentDevice) -> Option<(PhysicalDeviceKey, DeviceRoute)> {
        if !dev.online {
            return None;
        }
        let route = dev.route.clone()?;
        let identity = DeviceIdentity::from_parts(dev.serial.as_deref(), dev.unit_id);
        let physical_key = canonical_device_key(&stable_id(dev), Some(&identity))
            .or_else(|| PhysicalDeviceKey::parse(&dev.config_key))?;
        Some((physical_key, route))
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
        self.announce_low_batteries(&devices);
        // Volatile settings (lighting colour, sensor DPI, SmartShift, native
        // wheel mode) live in device RAM and reset on a power cycle. Every
        // reconnect shape re-applies the persisted values (#189): a first
        // sighting, a replug (new route), a wake from device sleep
        // (offline→online), or — via the forced budget — a system wake or a
        // device's own `0x1d4b` reconnect broadcast, where none of those
        // transitions are observable.
        let forced = self.forced_reapply_budget.take();
        let next_current = pick_current(&devices, self.config.selected_device());
        let rearm_capture =
            any_device_needs_capture_rearm(&self.devices, &devices, forced.is_some());
        let followup = std::mem::take(&mut self.reapply_followup);
        let (targets, next_followup) = plan_reapply(&self.devices, &devices, &followup, forced);
        self.reapply_followup = next_followup;
        // A keyboard switched to another platform on the device (a platform
        // key combination) is put back, as Logi Options+ does on its
        // platform-change report. So is one whose `0x4531` support shows up
        // only after it came online. A reapply below already covers both.
        let mut layout_checks = settings_moved(&self.devices, &devices);
        layout_checks.extend(os_layout_appeared(&self.devices, &devices));
        layout_checks.sort_unstable();
        layout_checks.dedup();
        for idx in layout_checks {
            if !targets.contains(&idx) {
                self.keep_os_layout(&devices[idx]);
            }
        }
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

    /// Show a lock key a keyboard turned on or off, unless the user turned the
    /// indicator off or this platform gives the key no meaning.
    pub fn announce_lock_key(&self, change: LockKeyChange) {
        let settings = &self.config.app_settings;
        if !settings.lock_key_notifications || !change.key.is_indicated_here() {
            return;
        }
        debug!(?change, "showing a lock key");
        self.observable
            .lock_keys()
            .show(change, settings.language.clone());
    }

    /// Post a low-battery notice for each online device whose battery has just
    /// come to need attention, while the user wants these notices. An offline
    /// device's reading is only its last known one, and says nothing new.
    /// While the notices are off, readings are still taken in, muted: a
    /// recharge in that time ends the device's discharge, and a battery that
    /// ran low meanwhile is announced on the first reading after they are
    /// turned on.
    fn announce_low_batteries(&mut self, devices: &[AgentDevice]) {
        let announcing = self.config.app_settings.low_battery_notifications;
        for device in devices.iter().filter(|device| device.online) {
            let Some(battery) = device.battery.as_ref() else {
                continue;
            };
            if !announcing {
                self.low_battery.observe_muted(&device.config_key, battery);
                continue;
            }
            if !self.low_battery.observe(&device.config_key, battery) {
                continue;
            }
            let name = self
                .config
                .device_custom_name(&device.config_key)
                .map(str::to_owned)
                .or_else(|| device.codename.as_deref().map(prettify_codename));
            info!(device = %device.config_key, ?battery, "announcing a low battery");
            self.observable.notices().post(
                NoticeKind::LowBattery {
                    name,
                    kind: device.kind,
                    battery: battery.clone(),
                },
                self.config.app_settings.language.clone(),
            );
        }
    }

    /// Force a volatile-settings re-apply for every online device on the next
    /// inventory refresh. Called on a detected system wake: the devices were
    /// likely power-cycled during the sleep, but the first post-wake snapshot
    /// can look identical to the last pre-sleep one (same set, same routes,
    /// already online), so the per-device transition triggers never fire.
    pub fn reapply_volatile_on_next_refresh(&mut self) {
        self.force_reapply(VOLATILE_REAPPLY_CONFIRM_RETRIES);
    }

    /// Force a volatile-settings re-apply on the next inventory refresh
    /// because a device broadcast its own reconnection (`0x1d4b`). The
    /// firmware is explicitly asking to be reconfigured, and — as after a
    /// system wake — the snapshot that follows can look identical to the last
    /// one (a nap short enough that no reconciliation ever observed the
    /// device offline), so the offline→online trigger cannot be relied on.
    /// The event channel is identity-free by design, so every online device
    /// re-applies; the writes are idempotent and the confirm budget is the
    /// short link-race one, not the boot ladder.
    pub fn reapply_reconnected_on_next_refresh(&mut self) {
        self.force_reapply(RECONNECT_REAPPLY_CONFIRM_RETRIES);
    }

    /// Coalesce forced re-apply requests, keeping the largest confirm budget:
    /// a device reconnect arriving between a system wake and its snapshot must
    /// not shrink the wake's boot-race ladder.
    fn force_reapply(&mut self, confirm_retries: u8) {
        self.forced_reapply_budget =
            Some(self.forced_reapply_budget.unwrap_or(0).max(confirm_retries));
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
        let settings = VolatileMouseSettings {
            wheel: self.wheel_mode_change(dev),
            dpi: device.and_then(|d| d.effective_dpi(&route_key)),
            report_rate: configured_report_rate(&self.config, dev),
            smartshift: device
                .and_then(|d| d.effective_smartshift(&route_key))
                .map(openlogi_hid::SmartShiftStatus::from),
        };
        if !settings.is_empty() {
            crate::hardware::reapply_mouse_volatile_in_background(
                &self.shared.device(&route),
                settings,
                &self.shared.report_rate_order,
                &self.shared.capture_plans,
            );
        }
        // A disabled entry is re-applied too: it writes black, and a device
        // whose firmware powers its lighting back on would otherwise ignore the
        // user's "off" after every reconnect.
        if let Some(lighting) = device.and_then(|d| d.effective_lighting(&route_key)) {
            crate::hardware::set_lighting_in_background(self.shared.device(&route), lighting);
        }
        if let Some(fn_lock) = self.config.fn_lock(key) {
            self.shared.write_fn_lock_in_background(&route, fn_lock);
        }
        self.keep_os_layout(dev);
        if let Some(capabilities) = dev.light_capabilities
            && let Some(light) = self.effective_light_settings(key)
        {
            self.shared
                .hardware
                .set_light_in_background(Some(route), &light, capabilities);
        }
    }

    /// Put `dev`'s keyboard on the host OS's key layout when the config keeps
    /// it there. Writes only when the current slot holds another platform.
    fn keep_os_layout(&self, dev: &AgentDevice) {
        if !dev.online || !keeps_os_layout(&self.config, dev) {
            return;
        }
        self.kept_os_layouts.keep(&dev.config_key);
        let (Some(route), Some(host)) = (dev.route.as_ref(), HostOs::current()) else {
            return;
        };
        crate::hardware::keep_os_layout_in_background(
            self.shared.device(route),
            host,
            Arc::clone(&self.kept_os_layouts),
            dev.config_key.clone(),
        );
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
            self.shared
                .hardware
                .set_light_in_background(dev.route.clone(), &light, capabilities);
            applied += 1;
        }
        info!(previous = ?previous, active, lights = applied, "applied camera-linked light state");
    }

    /// Resolve settings for reconnect/config re-application. A value changed
    /// on the light itself stands in for the saved one; camera policy and a
    /// transient manual override then replace only the effective power field.
    fn effective_light_settings(&self, key: &str) -> Option<LightSettings> {
        let mut light = self.config.light(key)?;
        if let Some(changed) = self.lights_changed_on_light.get(key) {
            light.apply_changes_on_light(*changed);
        }
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
            let Some(change) = self.wheel_mode_change(dev) else {
                debug!("no configured wheel mode fields — write skipped");
                continue;
            };
            crate::hardware::write_scroll_wheel_mode_in_background(
                self.shared.device(&route),
                change,
                self.shared.capture_plans.clone(),
            );
        }
    }

    /// The native wheel mode the agent's own writes (the reconnect re-apply
    /// and the reload push) give `dev`, or `None` for no write. That includes
    /// a wheel its capture session diverts: the session writes the same
    /// fields with its divert, and a write here would select native
    /// reporting under it.
    fn wheel_mode_change(&self, dev: &AgentDevice) -> Option<crate::hardware::WheelModeChange> {
        configured_wheel_mode(&self.config, dev, self.os_mouse_hook_available)
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
        let layout = ring.effective_layout(&self.config.apps, self.current_app.as_deref());
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
    /// the app changed in a way that alters bindings (the old or new app has a
    /// per-app override), so active button lifecycles must be canceled.
    pub fn set_current_app(&mut self, app: Option<ForegroundApp>) -> bool {
        let id = app.as_ref().map(|app| app.id.clone());
        self.observable.set_foreground(app);
        if id == self.current_app {
            return false;
        }
        let previous = std::mem::replace(&mut self.current_app, id);
        self.publish_hook_maps(self.hook_maps_for(self.current_key()));
        // Capture plans are app-scoped (per-app binding overlays); republish
        // them with the keyboard's effective bindings.
        self.publish_device_runtime();
        // Only a per-app override on either side changes what a held button
        // resolves to. Without one, keep the hold: a gesture that switches
        // desktops changes the front app and must not end its own hold (#1634).
        let overridden = |app: Option<&str>| {
            app.is_some_and(|app| {
                self.config
                    .devices
                    .keys()
                    .any(|key| self.config.has_app_override(key, app))
            })
        };
        overridden(previous.as_deref()) || overridden(self.current_app.as_deref())
    }

    /// Publish a pointer-window change separately from keyboard focus. The
    /// target travels in the same snapshot as its effective mouse bindings.
    /// Returns whether pointer-scoped presses must be canceled.
    pub fn set_pointer_context(&mut self, context: openlogi_hook::PointerContext) -> bool {
        if self.pointer_context == context {
            return false;
        }
        let previous = self.mouse_context().1;
        self.pointer_context = context;
        if self.config.app_settings.mouse_profile_target == MouseProfileTarget::Focused {
            return false;
        }
        self.publish_hook_maps(self.hook_maps_for(self.current_key()));
        self.publish_capture_plans();
        previous != self.mouse_context().1
    }

    /// Replace the config (after `config.toml` changed) and rebuild everything.
    /// The file is one the agent did not see saved, as at startup or after a
    /// hand edit (see [`ConfigSource::Loaded`]).
    pub fn reload_config(&mut self, config: Config) {
        self.reload_from(config, ConfigSource::Loaded);
    }

    /// [`Self::reload_config`] after the GUI saved `saved`, with `config`
    /// read from a file at revision `read`. A file still at the revision the
    /// GUI wrote is its save; another was edited since, and loads as
    /// [`Self::reload_config`] loads it (see [`ConfigSource::of_saved`]).
    pub fn reload_saved_config(
        &mut self,
        config: Config,
        read: ConfigRevision,
        saved: &SavedConfig,
    ) {
        self.reload_from(config, ConfigSource::of_saved(read, saved));
    }

    /// Replace the config from `source` and write each light it changed.
    fn reload_from(&mut self, config: Config, source: ConfigSource<'_>) {
        let writes = self.replace_config_from(config, source);
        self.write_lights(writes);
    }

    fn write_lights(&self, writes: Vec<LightWrite>) {
        for write in writes {
            self.shared.hardware.set_light_in_background(
                Some(write.route),
                &write.light,
                write.capabilities,
            );
        }
    }

    /// Everything [`Self::reload_config`] does except the light writes, which
    /// it returns instead: one per online light whose settings changed.
    #[cfg(test)]
    fn replace_config(&mut self, config: Config) -> Vec<LightWrite> {
        self.replace_config_from(config, ConfigSource::Loaded)
    }

    /// [`Self::replace_config`] for [`Self::reload_saved_config`].
    #[cfg(test)]
    fn replace_saved_config(&mut self, config: Config, chosen: &[ChosenLight]) -> Vec<LightWrite> {
        self.replace_config_from(config, ConfigSource::Saved { chosen })
    }

    fn replace_config_from(&mut self, config: Config, source: ConfigSource<'_>) -> Vec<LightWrite> {
        let previous_lights = self.effective_online_lights();
        let previous = std::mem::replace(&mut self.config, config);
        self.forget_saved_light_changes(&previous, source);
        // Parameter-only edits must not erase a transient manual choice while
        // the light remains camera-linked. Changing the policy invalidates it.
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
        self.apply_changed_fn_locks(&previous);
        self.apply_enabled_os_layouts(&previous);
        self.changed_lights(&previous_lights)
    }

    /// Put each keyboard whose "keep OS layout" just took effect — turned on,
    /// or its device re-enabled — on the host OS's layout. Turning it off
    /// leaves the keyboard's layout where it is, as in Logi Options+, and
    /// withdraws a layout write still queued for it.
    fn apply_enabled_os_layouts(&self, previous: &Config) {
        self.kept_os_layouts
            .retain(|key| config_keeps_os_layout(&self.config, key));
        for dev in newly_kept_os_layouts(previous, &self.config, &self.devices) {
            self.keep_os_layout(dev);
        }
    }

    /// Push a changed Fn-lock setting to the online keyboard it belongs to.
    /// Only the values that differ from `previous` are written, so an
    /// unrelated save never undoes an Fn+Esc the user pressed on the keyboard.
    /// The GUI toggle also writes directly through
    /// [`SharedHandles::set_fn_lock`]; both writes are ordered per keyboard,
    /// so the newest request is the one that stays. The reconnect path is
    /// [`Self::reapply_volatile_settings`].
    fn apply_changed_fn_locks(&self, previous: &Config) {
        for dev in self.devices.iter().filter(|dev| dev.online) {
            let Some(route) = dev.route.clone() else {
                continue;
            };
            let fn_lock = self.config.fn_lock(&dev.config_key);
            if fn_lock == previous.fn_lock(&dev.config_key) {
                continue;
            }
            if let Some(fn_lock) = fn_lock {
                self.shared.write_fn_lock_in_background(&route, fn_lock);
            }
        }
    }

    /// The effective settings of every online standalone light, keyed by its
    /// config key: what [`Self::changed_lights`] compares a reload against.
    fn effective_online_lights(&self) -> HashMap<String, LightSettings> {
        self.online_lights()
            .filter_map(|(dev, _, _)| {
                let light = self.effective_light_settings(&dev.config_key)?;
                Some((dev.config_key.clone(), light))
            })
            .collect()
    }

    /// The online lights a config reload has to write: those whose effective
    /// settings now send other commands than they did in `previous`. Only a
    /// changed light is written, so an unrelated save never undoes a change
    /// made on the light itself — the same rule as
    /// [`Self::apply_changed_fn_locks`]. A flipped camera policy or a dropped
    /// manual override changes the effective power, so it is written too.
    fn changed_lights(&self, previous: &HashMap<String, LightSettings>) -> Vec<LightWrite> {
        self.online_lights()
            .filter_map(|(dev, route, capabilities)| {
                let light = self.effective_light_settings(&dev.config_key)?;
                let unchanged = previous.get(&dev.config_key).is_some_and(|before| {
                    commands_for_light_settings(*before, capabilities)
                        == commands_for_light_settings(light, capabilities)
                });
                (!unchanged).then(|| LightWrite {
                    route: route.clone(),
                    light,
                    capabilities,
                })
            })
            .collect()
    }

    /// Every online device with a route and standalone-light controls.
    fn online_lights(
        &self,
    ) -> impl Iterator<Item = (&AgentDevice, &DeviceRoute, LightCapabilities)> {
        self.devices
            .iter()
            .filter(|dev| dev.online)
            .filter_map(|dev| Some((dev, dev.route.as_ref()?, dev.light_capabilities?)))
    }
}

/// One standalone-light write a config reload queues.
#[derive(Debug, PartialEq)]
struct LightWrite {
    route: DeviceRoute,
    light: LightSettings,
    capabilities: LightCapabilities,
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
/// A keyboard that could hold the key-capture session: it is enabled, has a
/// route, and at least one of its keys is bound.
struct KeyboardCandidate<'a> {
    dev: &'a AgentDevice,
    bindings: BTreeMap<ButtonId, Binding>,
    divert: KeyboardDivertSet,
}

/// The `0x1b04` controls a keyboard's bindings ask to divert, split into the
/// ones the session will arm and the reserved ones it refuses.
#[derive(Debug, Default, PartialEq, Eq)]
struct KeyboardDivertSet {
    wanted: BTreeMap<u16, ButtonId>,
    reserved: Vec<u16>,
}

/// Which of `bindings` carry a stored binding on a keyboard control — a
/// control is diverted whenever it is present at all, even as
/// [`openlogi_core::binding::Action::None`] (which means "suppress this key," not "leave native"):
/// `button_bindings_for` seeds no default for a [`ButtonId::Control`], so
/// presence here is always an explicit user choice (#1747).
fn keyboard_divert_set(bindings: &BTreeMap<ButtonId, Binding>) -> KeyboardDivertSet {
    let mut set = KeyboardDivertSet::default();
    for button in bindings.keys() {
        let Some(cid) = button.cid().map(openlogi_core::binding::Cid::raw) else {
            continue;
        };
        // A keyboard key never seeds a default (see `ButtonId::ALL`'s own
        // doc), so its presence here means the user explicitly bound it —
        // even to `Action::None`, which means "suppress", not "leave
        // native" (#1747). Only a key truly absent from the stored config
        // is left on its firmware function.
        if is_reserved_keyboard_control(cid) {
            set.reserved.push(cid);
        } else {
            set.wanted.insert(cid, *button);
        }
    }
    set
}

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
