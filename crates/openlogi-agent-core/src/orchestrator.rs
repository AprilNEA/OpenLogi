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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{
    ActionRingIcon, Binding, ButtonId, GamingButtons, GamingLayout, GridKey, KeypadPage,
};
use openlogi_core::bindings::{bindings_on_page, button_bindings_for, oshook_gestures_for};
use openlogi_core::config::{
    AppSettings, Config, ConfigRevision, LightSettings, MouseProfileTarget, canonical_device_key,
};
use openlogi_core::device::{
    BatteryInfo, Capabilities, ChangedOnLight, DeviceInventory, DeviceKind, LightCapabilities,
    LightState, StandaloneDevice, prettify_codename,
};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId, PhysicalDeviceKey};
use openlogi_hid::{
    BacklightChange, BacklightState, CaptureChannelSlot, ChannelPool, ChannelRegistry,
    DeviceIoGate, DeviceRoute, DisableKeysMask, Dpi, EasySwitchHosts, FnLockState,
    ForceSensingInfo, HidppOperation, HostOs, PointerSpeed, ReportRate, ReportRateInfo, WriteError,
    commands_for_light_settings, is_reserved_keyboard_control, key_display_for,
};
#[cfg(test)]
use openlogi_ipc::ChosenLight;
use openlogi_ipc::{InventoryHealth, LockKeyChange, NoticeKind, SavedConfig};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::action_ring::ActionRingSessionSpec;
use crate::capture_plan::{
    CapturePublication, PlanDevice, SharedCapturePlans, hidpp_side_gesture_maps_for,
    plan_for_device, shift_bindings_for,
};
use crate::hardware::{
    DeviceAccess, DeviceOp, HardwareContext, KeptOsLayouts, VolatileMouseSettings, WriteOrder,
    show_precision_led, write_pointer_speed_in_background,
};
use crate::notices::LowBatteryWatch;
use crate::observable::ObservableState;
use crate::pointer_speed::{AppWrite, ReloadChange, SavedSpeed, SpeedMouse, SpeedWrite};
use crate::receiver_access::ReceiverAccess;
use crate::runtime::PageTurn;
use crate::runtime::hook::{HookMaps, SharedHookMaps};
use crate::runtime::scroll::ScrollPreferences;
use crate::watchers::host_switch::{HostSwitchLink, HostSwitchLinks};
use crate::watchers::inventory::InventoryRefresh;
use crate::watchers::key_display::{KeyDisplaySpec, KeyFace, SharedKeyDisplaySpecs};
use crate::watchers::keyboard::{KeyboardSpec, SharedKeyboardSpecs};
use crate::watchers::light::{WatchedLight, WatchedLights};
use crate::{DpiCycleState, DpiCycles, PointerSpeeds};

mod devices;
mod lights;

pub use lights::{ConfigSource, LightWriteClaim, TakenLightChange, write_light};

use devices::reapply_targets;
use devices::{
    FollowerCookieState, FollowerCookieWrite, RECONNECT_REAPPLY_CONFIRM_RETRIES,
    VOLATILE_REAPPLY_CONFIRM_RETRIES, assignments_changed, build_devices, bump_settings_epoch,
    carry_lifecycle_budgets, changed_follower_cookies, changed_force_sensing,
    config_keeps_os_layout, configured_dpi, configured_force_sensing, configured_report_rate,
    configured_wheel_mode, host_switch_links, is_hidpp_device, keeps_os_layout, main_wheel_divert,
    newly_kept_os_layouts, os_layout_appeared, pick_current, plan_disabled_keys_recovery,
    plan_reapply, published_inventories, reconnect_follower_cookies, same_device_lifecycle,
    saved_speed, settings_moved, stable_id, wheel_inverted,
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

impl AgentDevice {
    /// A reload may adopt the remembered route into a physical key without
    /// another inventory pass. Resolve against the supplied config, not the
    /// key cached when this inventory record was built.
    fn resolve_config_key(&self, config: &Config) -> Option<PhysicalDeviceKey> {
        let identity = DeviceIdentity::from_parts(self.serial.as_deref(), self.unit_id);
        config.resolve_device_key(&stable_id(self), self.online.then_some(&identity))
    }
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
    /// The keyboard key-capture watcher's targets + bindings: one per enabled
    /// keyboard-family device with bound keys, empty while none has any.
    pub keyboard_specs: SharedKeyboardSpecs,
    /// The keyboard capture sessions' open channel, reused by Fn-lock writes
    /// (the mouse-oriented [`Self::capture_channel`] points elsewhere). Last
    /// writer wins; a write uses it only when it matches the write's route.
    pub keyboard_channel: CaptureChannelSlot,
    /// What every online keypad's LCD keys should show; empty while no
    /// keypad with display keys is present.
    pub key_display_specs: SharedKeyDisplaySpecs,
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
    /// Precision mode of the mice whose speed `0x2205` scales, and the order
    /// of every path's pointer-speed writes.
    pointer_speeds: Arc<PointerSpeeds>,
    /// Orders every path's press-force writes per force-sensing device.
    force_sensing_order: WriteOrder,
    /// Orders the GUI's backlight writes per keyboard.
    backlight_order: WriteOrder,
    /// Orders host renames per device.
    host_name_order: WriteOrder,
    /// Orders the `ChangeHost` cookie writes per linked device.
    follower_cookie_order: WriteOrder,
    /// Saved policy and manual writes share intent identity, not retry order.
    disabled_keys_order: WriteOrder<Option<DisableKeysMask>>,
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

    /// The precision-mode state the action runtime toggles.
    #[must_use]
    pub fn pointer_speeds(&self) -> Arc<PointerSpeeds> {
        Arc::clone(&self.pointer_speeds)
    }

    /// Write `speed` to the mouse at `route` and return what it reads back.
    /// The write ends the mouse's precision mode, so the pointer moves at the
    /// speed the settings app shows, and darkens the DPI LED when the
    /// precision speed ends with it. It is ordered with the precision button
    /// and the reconnect reapply as [`Self::set_report_rate`] is ordered:
    /// a write superseded before its turn reads the mouse instead.
    pub async fn set_pointer_speed(
        &self,
        route: &DeviceRoute,
        speed: PointerSpeed,
    ) -> Result<PointerSpeed, WriteError> {
        let AppWrite { ticket, restore } = self
            .pointer_speeds
            .app_write(route, SavedSpeed::Scaled(Some(speed)));
        let speeds = Arc::clone(&self.pointer_speeds);
        self.device(route)
            .run_in_turn(
                HidppOperation::WritePointerScaling,
                &ticket,
                |c| async move {
                    let started = tokio::time::Instant::now();
                    let applied = openlogi_hid::set_pointer_speed_on(&c, speed).await?;
                    if let Some(restore) = restore {
                        show_precision_led(&c, false, started).await;
                        speeds.restored(&restore);
                    }
                    Ok(applied)
                },
                |c| async move { openlogi_hid::get_pointer_speed_on(&c).await },
            )
            .await
    }

    /// Write `force` to the force-sensing button of the device at `route`
    /// and return what it reads back. When a newer press-force write for the
    /// device is requested before this one gets its turn, this one reads the
    /// device instead of writing a force that is about to be replaced; its
    /// `Ok` then carries the device's force, which need not be `force`.
    /// Waiting for the turn does not count against the write's deadline.
    pub async fn set_force_sensing(
        &self,
        route: &DeviceRoute,
        force: u16,
    ) -> Result<ForceSensingInfo, WriteError> {
        let ticket = self.force_sensing_order.request(route);
        self.device(route)
            .run_in_turn(
                HidppOperation::WriteForceSensing,
                &ticket,
                |c| async move { openlogi_hid::set_force_sensing_on(&c, force).await },
                |c| async move { openlogi_hid::get_force_sensing_on(&c).await },
            )
            .await
    }

    /// Write `dpi` to the mouse at `route`. On a DPI mouse the write ends
    /// precision mode, so the pointer moves at the DPI the settings app
    /// shows, and darkens the DPI LED when the precision speed ends with it.
    /// It is ordered with the precision button, the DPI preset actions and
    /// the reconnect reapply. A write superseded before its turn writes
    /// nothing.
    pub async fn set_dpi(&self, route: &DeviceRoute, dpi: Dpi) -> Result<(), WriteError> {
        let AppWrite { ticket, restore } = self
            .pointer_speeds
            .app_write(route, SavedSpeed::Dpi(Some(dpi)));
        // A DPI shift returns here, not to the last preset picked. Recorded
        // when the write is requested, as a preset pick is, so a pick made
        // while this write runs is not overwritten when it finishes.
        if let Ok(mut cycles) = self.dpi_cycle.write() {
            cycles.reapplied_at(route, dpi);
        }
        let speeds = Arc::clone(&self.pointer_speeds);
        let result = self
            .device(route)
            .run_in_turn(
                HidppOperation::WriteDpi,
                &ticket,
                |c| async move {
                    let started = tokio::time::Instant::now();
                    openlogi_hid::set_dpi_on(&c, dpi).await?;
                    if let Some(restore) = restore {
                        show_precision_led(&c, false, started).await;
                        speeds.restored(&restore);
                    }
                    Ok(())
                },
                |_| async { Ok(()) },
            )
            .await;
        // A rejected write leaves the mouse where it was, which only a
        // reading of it can tell.
        if result.is_err()
            && let Ok(mut cycles) = self.dpi_cycle.write()
        {
            cycles.rejected_at(route, dpi);
        }
        result
    }

    /// Apply `change` to the backlight of the keyboard at `route` and return
    /// what it reads back. Every change is applied, one at a time in request
    /// order: each rewrites the whole configuration it read, so two that
    /// overlapped would undo one another, and a newer change to another
    /// control does not make an older one moot. Waiting for the earlier
    /// changes does not count against the write's deadline. Reserves its
    /// place before returning, so an owned task cannot reorder admission by
    /// polling a later request first.
    pub fn set_backlight(
        &self,
        route: &DeviceRoute,
        change: BacklightChange,
    ) -> impl Future<Output = Result<BacklightState, WriteError>> + Send + 'static + use<> {
        let ticket = self.backlight_order.request(route);
        let device = self.device(route);
        async move {
            device
                .run_in_order(
                    HidppOperation::WriteBacklight,
                    ticket,
                    move |c| async move { openlogi_hid::update_backlight_on(&c, change).await },
                )
                .await
        }
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

    /// Replace disabled keys, retaining priority over old reconnect attempts
    /// until config acknowledges the confirmed choice. Failed/cancelled RPCs
    /// release only their own pending intent.
    pub async fn set_disable_keys(
        &self,
        route: &DeviceRoute,
        desired: DisableKeysMask,
    ) -> Result<openlogi_hid::DisableKeysState, WriteError> {
        let refresh = Arc::clone(&self.inventory_refresh);
        let pending = self
            .disabled_keys_order
            .begin(route, Some(desired))
            .on_abandoned(move || {
                let refresh = refresh.read().unwrap_or_else(PoisonError::into_inner);
                if let Some(refresh) = refresh.as_ref() {
                    refresh.request_settings_confirmation();
                }
            });
        crate::hardware::write_manual_disable_keys(self.keyboard_device(route), pending, desired)?
            .wait()
            .await
    }

    /// Read after previously admitted Disable Keys workers finish, including
    /// background policy writes queued by config reload. This does not create
    /// a new user intent or block independent routes/settings.
    pub async fn read_disable_keys(
        &self,
        route: &DeviceRoute,
    ) -> Result<openlogi_hid::DisableKeysState, WriteError> {
        let reservation = self.disabled_keys_order.reserve(route);
        reservation.wait().await;
        self.keyboard_device(route)
            .run(HidppOperation::ReadDisableKeys, |channel| async move {
                openlogi_hid::get_disable_keys_on(&channel).await
            })
            .await
    }

    fn reapply_disabled_keys(&self, route: &DeviceRoute) -> Option<std::thread::JoinHandle<()>> {
        let (desired, ticket) = self.disabled_keys_order.policy(route)?;
        crate::hardware::write_disabled_keys_in_background(
            self.keyboard_device(route),
            ticket,
            desired?,
        )
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

    /// [`Self::set_force_sensing`] without waiting, for the config-reload and
    /// reconnect paths; the outcome is logged.
    fn write_force_sensing_in_background(&self, route: &DeviceRoute, force: u16) {
        crate::hardware::write_force_sensing_in_background(
            self.device(route),
            self.force_sensing_order.request(route),
            force,
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
    /// System resume must rearm every online capture; an identity-free device
    /// wake only reapplies settings, since each armed listener owns its wake.
    forced_capture_rearm: bool,
    /// Rearm epochs belong to exact routes. Discovery or wake on one link
    /// must not retire a still-running capture on another link.
    capture_route_generations: HashMap<DeviceRoute, u64>,
    /// Each keypad route's LCD panel epoch, advanced with every re-apply of
    /// its volatile settings: the images live in device RAM like them, and a
    /// reconnect the snapshot cannot show (a `0x1d4b` broadcast, a replaced
    /// channel) re-applies settings without moving any capture epoch.
    panel_generations: HashMap<DeviceRoute, u64>,
    /// The last panel epoch handed out.
    panel_epoch: u64,
    /// Whether the last enumeration pass failed to open HID++ nodes; published
    /// atomically with the inventory so no observation pairs a fresh device
    /// set with a stale flag.
    hid_open_failures: bool,
    /// Physical identities of devices first sighted (or targeted after wake)
    /// recently, with remaining confirming re-apply budget: the first write can
    /// race the device's own boot or reconnect and be lost.
    reapply_followup: BTreeMap<DeviceStableId, u8>,
    /// Physical identities of keyboards whose failed or cancelled manual
    /// Disable Keys request needs bounded recovery. This plan excludes volatile
    /// settings that are unrelated to the manual request.
    disabled_keys_recovery: BTreeMap<DeviceStableId, u8>,
    /// Agent-owned settings changes on each route, added to the published
    /// [`PairedDevice::settings_revision`](openlogi_core::device::PairedDevice::settings_revision).
    /// Disable Keys recovery and backlight writes can change the keyboard
    /// without a firmware broadcast, so clients need this move to read again.
    settings_epochs: Vec<(DeviceRoute, u64)>,
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
    /// The cookie clears that wait for their linked device to be online, and
    /// the last cookie byte seen for each keyboard. Shared with the cookie
    /// writes, which return a clear that does not finish.
    follower_cookies: Arc<Mutex<FollowerCookieState>>,
    /// Each keypad's current binding page, by config key — runtime state the
    /// page actions move and nothing persists: a fresh agent starts every
    /// keypad on page 1. Read through [`Self::keypad_page`], which clamps a
    /// page the config has since removed.
    keypad_pages: BTreeMap<String, KeypadPage>,
    /// Whether the OS mouse hook is currently installed. Back/Forward gesture
    /// motion comes from HID++, but diversion is published only while the
    /// broader mouse-remapping path is available so losing the hook leaves the
    /// side buttons native.
    os_mouse_hook_available: bool,
    /// Private producer halves for the read-only runtime projections in
    /// `shared`, keeping the orchestrator's single-writer contract structural.
    capture_plans_tx: watch::Sender<Arc<CapturePublication>>,
    keyboard_specs_tx: watch::Sender<Arc<Vec<KeyboardSpec>>>,
    key_display_specs_tx: watch::Sender<Arc<Vec<KeyDisplaySpec>>>,
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
        let (keyboard_specs_tx, keyboard_specs) = watch::channel(Arc::new(Vec::new()));
        let (key_display_specs_tx, key_display_specs) = watch::channel(Arc::new(Vec::new()));
        let (host_switch_links_tx, host_switch_links) = watch::channel(Arc::new(Vec::new()));
        let (watched_lights_tx, watched_lights) = watch::channel(Arc::new(Vec::new()));
        openlogi_inject::set_desktop_switch(desktop_switch(&config.app_settings));
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
            keyboard_specs,
            keyboard_channel: Arc::new(RwLock::new(None)),
            key_display_specs,
            capture_rearm_generation: Arc::new(AtomicU64::new(0)),
            receiver_access: ReceiverAccess::default(),
            transport_turns: crate::hardware::TransportTurns::default(),
            host_switch_links,
            watched_lights,
            fn_lock_order: WriteOrder::default(),
            report_rate_order: WriteOrder::default(),
            pointer_speeds: Arc::default(),
            force_sensing_order: WriteOrder::default(),
            backlight_order: WriteOrder::default(),
            host_name_order: WriteOrder::default(),
            follower_cookie_order: WriteOrder::default(),
            disabled_keys_order: WriteOrder::default(),
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
            forced_capture_rearm: false,
            capture_route_generations: HashMap::new(),
            panel_generations: HashMap::new(),
            panel_epoch: 0,
            hid_open_failures: false,
            reapply_followup: BTreeMap::new(),
            disabled_keys_recovery: BTreeMap::new(),
            settings_epochs: Vec::new(),
            camera_active: None,
            manual_light_overrides: BTreeMap::new(),
            light_states: BTreeMap::new(),
            lights_changed_on_light: BTreeMap::new(),
            light_writes: BTreeMap::new(),
            next_light_write: 0,
            kept_os_layouts: Arc::default(),
            follower_cookies: Arc::default(),
            keypad_pages: BTreeMap::new(),
            os_mouse_hook_available: false,
            capture_plans_tx,
            keyboard_specs_tx,
            key_display_specs_tx,
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
        // The OS hook only sees standard HID buttons, whose defaults no
        // G-series layout changes, so this map needs no layout.
        let mut bindings = button_bindings_for(&self.config, key, app, None);
        let mut gestures = oshook_gestures_for(&self.config, key, app);
        if cfg!(target_os = "macos")
            && let Some(key) = key
            && !self.is_spy_only(key)
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
        let shift_bindings = key
            .map(|key| shift_bindings_for(&self.config, key))
            .unwrap_or_default();
        HookMaps {
            bindings,
            gestures,
            pointer_target,
            selected_device: key.map(str::to_owned),
            shift_bindings,
            ..HookMaps::default()
        }
    }

    /// Whether `key` is a G-series mouse: one with a layout, or any other
    /// without ReprogControls. It has no `0x1b04` raw XY, so its gesture
    /// buttons stay with the OS hook.
    fn is_spy_only(&self, key: &str) -> bool {
        self.devices
            .iter()
            .find(|dev| dev.config_key == key)
            .is_some_and(|dev| {
                GamingButtons::for_device(&dev.model_key, dev.capabilities.as_ref()).is_some()
            })
    }

    /// Publish hook maps while preserving what hardware capture sessions
    /// reported: thumb-wheel polarities, diverted buttons and held G-Shift
    /// buttons. Selection, polarity, and bindings share the one lock the
    /// callback reads, so a device switch cannot combine facts from two
    /// devices.
    fn publish_hook_maps(&self, mut maps: HookMaps) {
        match self.shared.hook_maps.write() {
            Ok(mut current) => {
                maps.thumbwheel_positive_is_forward =
                    std::mem::take(&mut current.thumbwheel_positive_is_forward);
                maps.shift_held = std::mem::take(&mut current.shift_held);
                maps.diverted_buttons = std::mem::take(&mut current.diverted_buttons);
                *current = maps;
            }
            Err(error) => {
                warn!(%error, lock = "hook_maps", "lock poisoned — keeping stale value");
            }
        }
    }

    /// The key-capture spec of every enabled keyboard-family device that
    /// binds a key in some profile (an unbound key must never be diverted),
    /// at most one per route: each gets its own capture session, so an MX
    /// Keys and an MX Creative Keypad — or two keyboards on one receiver —
    /// capture at once.
    ///
    /// A keyboard that binds keys only for some apps still holds its session
    /// while another app is in front, with an empty capture set. The session
    /// then diverts and hands back keys in place as the app in front changes,
    /// instead of being armed from scratch on every focus change (#2140).
    ///
    /// The bound keys *are* the capture set: every
    /// [`ButtonId::Control`] in a keyboard's effective bindings names the
    /// `0x1b04` control to divert, and its capture session arms whichever of
    /// those the device's own control table reports as divertable. No fixed key
    /// table sits in between, so a keyboard OpenLogi has never seen is
    /// remappable the day it ships. [`ButtonId::Grid`] bindings join the same
    /// session as the observed keypad grid keys — same binding gate, no
    /// firmware writes.
    ///
    /// A stale receiver slot for the same model (#1581) carries no bindings,
    /// so it publishes no spec and never shadows the keyboard that is
    /// actually typing. Beyond that a spec deliberately does NOT require the
    /// keyboard to be online: an idle keyboard sleeps within minutes and
    /// probe timeouts can flap it offline, and tearing its capture session
    /// down on every nap would hand the diverted keys back to the firmware
    /// (dead bindings) until the re-arm races through. The session instead
    /// stays up across sleeps — its channel is to the always-present receiver
    /// — and re-arms diversion on the device's `0x1d4b` reconnection
    /// broadcast.
    ///
    /// Two inventory entries can share a route — a direct route names a
    /// VID/PID, so two identical wired devices are indistinguishable there.
    /// One that captures keys for the app in front wins that route over one
    /// that captures keys only for other apps, then the online one, then
    /// inventory order.
    fn keyboard_specs_for(&self) -> Vec<KeyboardSpec> {
        let candidates = self
            .devices
            .iter()
            .filter(|d| d.kind.is_keyboard_family())
            .filter(|d| self.config.device_enabled(&d.config_key))
            .filter(|d| binds_keys_in_some_profile(&self.config, &d.config_key))
            .filter_map(|dev| {
                let route = dev.route.clone()?;
                let stored = self.foreground_key_bindings(&dev.config_key);
                // Capture from every page; dispatch the current page's.
                let divert = keyboard_divert_set(&stored);
                let bindings = self.current_page_of(&dev.config_key, &stored);
                Some((
                    KeyboardCandidate {
                        dev,
                        bindings,
                        divert,
                    },
                    route,
                ))
            });
        let rank = |candidate: &KeyboardCandidate<'_>| {
            (candidate.divert.captures_nothing(), !candidate.dev.online)
        };
        let mut chosen: Vec<(KeyboardCandidate<'_>, DeviceRoute)> = Vec::new();
        for (candidate, route) in candidates {
            match chosen.iter_mut().find(|(_, taken)| *taken == route) {
                Some(entry) if rank(&candidate) < rank(&entry.0) => {
                    *entry = (candidate, route);
                }
                Some(_) => {}
                None => chosen.push((candidate, route)),
            }
        }
        let mut specs = Vec::with_capacity(chosen.len());
        for (chosen, route) in chosen {
            for &cid in &chosen.divert.reserved {
                warn!(
                    cid = format_args!("{cid:#06x}"),
                    key = %chosen.dev.config_key,
                    "binding names a control OpenLogi never diverts as a key — left native"
                );
            }
            specs.push(KeyboardSpec {
                config_key: chosen.dev.config_key.clone(),
                route,
                wanted: chosen.divert.wanted,
                grid: chosen.divert.grid,
                bindings: chosen.bindings,
            });
        }
        specs
    }

    /// `config_key`'s stored bindings for the foreground app, across every
    /// keypad page.
    fn foreground_key_bindings(&self, config_key: &str) -> BTreeMap<ButtonId, Binding> {
        button_bindings_for(
            &self.config,
            Some(config_key),
            self.current_app.as_deref(),
            None,
        )
    }

    /// The part of `stored` that `config_key`'s keys act on now: its current
    /// keypad page.
    fn current_page_of(
        &self,
        config_key: &str,
        stored: &BTreeMap<ButtonId, Binding>,
    ) -> BTreeMap<ButtonId, Binding> {
        bindings_on_page(stored, self.keypad_page(config_key))
    }

    /// The binding page `config_key`'s keypad is on: the last page turned to,
    /// or page 1 — also when the config has since removed that page.
    fn keypad_page(&self, config_key: &str) -> KeypadPage {
        self.keypad_pages
            .get(config_key)
            .copied()
            .filter(|page| *page <= self.config.keypad_pages(config_key))
            .unwrap_or(KeypadPage::FIRST)
    }

    /// Move `device_key`'s keypad one page in `turn`'s direction, wrapping
    /// within its configured pages, and republish what its keys dispatch and
    /// show. A device with a single page (or no key) has nothing to turn.
    pub fn turn_keypad_page(&mut self, device_key: Option<&str>, turn: PageTurn) {
        let Some(key) = device_key else {
            debug!("keypad page turn without a device — ignored");
            return;
        };
        let last = self.config.keypad_pages(key);
        if last == KeypadPage::FIRST {
            debug!(key, "keypad page turn on a single-page device — ignored");
            return;
        }
        let current = self.keypad_page(key);
        let next = match turn {
            PageTurn::Previous => current.previous_within(last),
            PageTurn::Next => current.next_within(last),
        };
        info!(key, page = %next, of = %last, "keypad page turned");
        self.keypad_pages.insert(key.to_owned(), next);
        self.publish_device_runtime();
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
        self.publish_pointer_speeds(self.current_key());
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
        publish_arc_if_changed(&self.keyboard_specs_tx, self.keyboard_specs_for());
        publish_arc_if_changed(&self.watched_lights_tx, self.watched_lights());
        self.publish_lights();
        publish_arc_if_changed(&self.key_display_specs_tx, self.key_display_specs_for());
    }

    /// What each online, enabled keypad's LCD keys should show: the glyph of
    /// the action each grid key is bound to, or nothing for an unbound key —
    /// the same effective bindings (per-app overlay included) the keyboard
    /// capture session dispatches, under the same "bound" rule.
    ///
    /// Only a device whose panel the registry describes qualifies; no kind
    /// gate stands in for that identity fact.
    fn key_display_specs_for(&self) -> Vec<KeyDisplaySpec> {
        self.devices
            .iter()
            .filter(|dev| dev.online && self.config.device_enabled(&dev.config_key))
            .filter_map(|dev| {
                let route = dev.route.clone()?;
                let display = key_display_for(&route)?;
                let bindings = self.current_page_of(
                    &dev.config_key,
                    &self.foreground_key_bindings(&dev.config_key),
                );
                let faces = GridKey::ALL
                    .into_iter()
                    .map(|key| (key, key_face(bindings.get(&ButtonId::grid(key)))))
                    .collect();
                let generation = self.panel_generations.get(&route).copied().unwrap_or(0);
                Some(KeyDisplaySpec {
                    route,
                    display,
                    generation,
                    faces,
                })
            })
            .collect()
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
            let mut presets = self.config.dpi_presets(&dev.config_key);
            if presets.is_empty()
                && let Some(layout) = GamingLayout::for_model_key(&dev.model_key)
            {
                presets = layout
                    .stock_dpi_presets
                    .iter()
                    .copied()
                    .map(Dpi::new)
                    .collect();
            }
            let committed = configured_dpi(&self.config, dev);
            let previous = guard
                .by_key
                .get(&dev.config_key)
                .filter(|state| state.committed == committed);
            let current = previous.map_or(committed, |state| state.current);
            let before_shift = guard
                .by_key
                .get(&dev.config_key)
                .map(|state| state.before_shift.clone())
                .unwrap_or_default();
            let nearest = current.and_then(|dpi| crate::dpi::nearest_preset(&presets, dpi));
            let same_presets = previous.filter(|state| state.presets == presets);
            by_key.insert(
                dev.config_key.clone(),
                DpiCycleState {
                    index: same_presets.map_or_else(|| nearest.unwrap_or(0), |state| state.index),
                    capabilities: same_presets.and_then(|state| state.capabilities.clone()),
                    presets,
                    target: Some(route),
                    current,
                    committed,
                    before_shift,
                },
            );
        }
        guard.selected = selected.map(str::to_owned);
        guard.by_key = by_key;
    }

    /// Publish every online, enabled mouse whose speed OpenLogi sets, for
    /// the precision button. The precision mode of each mouse is kept.
    fn publish_pointer_speeds(&self, selected: Option<&str>) {
        self.shared
            .pointer_speeds
            .publish(selected, self.speed_mice());
    }

    /// Every online, enabled mouse whose speed OpenLogi sets, with how it
    /// sets it and the value saved for it.
    fn speed_mice(&self) -> impl Iterator<Item = SpeedMouse> + '_ {
        self.devices
            .iter()
            .filter(|dev| dev.online && self.config.device_enabled(&dev.config_key))
            .filter_map(|dev| {
                Some(SpeedMouse {
                    key: dev.config_key.clone(),
                    route: dev.route.clone()?,
                    saved: saved_speed(&self.config, dev)?,
                })
            })
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
        let keyboards = self.keyboard_specs_for();
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
            let rearm_generation = self
                .capture_route_generations
                .get(&route)
                .copied()
                .unwrap_or(0);
            let (app, pointer_target) = if dev.kind.is_keyboard_family() {
                (self.current_app.as_deref(), None)
            } else {
                self.mouse_context()
            };
            let layout = GamingLayout::for_model_key(&dev.model_key);
            let mut plan = plan_for_device(
                &self.config,
                PlanDevice {
                    physical_key,
                    config_key: &dev.config_key,
                    route,
                    layout,
                },
                app,
                rearm_generation,
                self.os_mouse_hook_available,
            );
            plan.dispatch.pointer_target = pointer_target;
            plan.dispatch.invert_wheel = wheel_inverted(&self.config, dev);
            plan.target.spec.divert_main_wheel =
                main_wheel_divert(&self.config, dev, self.os_mouse_hook_available);
            let mut plan = plan.with_onboard(&self.config, layout);
            if let Some(keyboard) = keyboards
                .iter()
                .find(|keyboard| keyboard.route == plan.target.route)
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
        self.refresh_inventory_inner(inventories, standalone, hid_open_failures, false);
    }

    /// Apply the inventory pass whose delayed purpose is confirming volatile
    /// settings. Only this path consumes one bounded confirmation attempt;
    /// ordinary HID and hotplug snapshots may arrive much sooner and must not
    /// exhaust the retry run while a device's feature path is still booting.
    pub fn refresh_inventory_for_settings_confirmation(
        &mut self,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        hid_open_failures: bool,
    ) {
        self.refresh_inventory_inner(inventories, standalone, hid_open_failures, true);
    }

    fn refresh_inventory_inner(
        &mut self,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        hid_open_failures: bool,
        confirm_reapply: bool,
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
        // Retire departing routes before forgetting them. Offline routes that
        // remain in inventory still observe config changes without doing I/O.
        for old in &self.devices {
            if let Some(route) = &old.route
                && !devices.iter().any(|dev| same_device_lifecycle(old, dev))
            {
                self.shared.disabled_keys_order.retire(route);
            }
        }
        self.disabled_keys_recovery =
            carry_lifecycle_budgets(&self.devices, &devices, &self.disabled_keys_recovery);
        for dev in &devices {
            self.sync_disabled_keys_policy(dev);
        }
        self.announce_low_batteries(&devices);
        // Before any cookie plan below: a keyboard that goes to sleep or
        // leaves later keeps its byte for the clears and links that name it.
        self.follower_cookies().remember_leads(&devices);
        // Volatile settings (lighting colour, sensor DPI, SmartShift, native
        // wheel mode) live in device RAM and reset on a power cycle. Every
        // reconnect shape re-applies the persisted values (#189): a first
        // sighting, a replug (new route), a wake from device sleep
        // (offline→online), or — via the forced budget — a system wake, a
        // device's own `0x1d4b` reconnect broadcast, or a channel the
        // enumerator replaced after retiring it, where none of those
        // transitions are observable.
        let forced = self.forced_reapply_budget.take();
        let next_current = pick_current(&devices, self.config.selected_device());
        self.rearm_capture_routes(&devices);
        self.repaint_panels(&devices, forced.is_some());
        // Manual cancellation can happen long after the boot/reconnect ladder
        // drained. Its owner requests only a Disable Keys recovery, and does
        // not consume a request while a successor still owns the setting.
        for dev in &devices {
            if dev.online
                && let Some(route) = &dev.route
                && self.shared.disabled_keys_order.schedule_recovery(route)
            {
                self.disabled_keys_recovery
                    .entry(stable_id(dev))
                    .and_modify(|remaining| {
                        *remaining = (*remaining).max(VOLATILE_REAPPLY_CONFIRM_RETRIES);
                    })
                    .or_insert(VOLATILE_REAPPLY_CONFIRM_RETRIES);
            }
        }
        let disabled_keys_recovery = std::mem::take(&mut self.disabled_keys_recovery);
        let (disabled_keys_targets, next_disabled_keys_recovery) =
            plan_disabled_keys_recovery(&devices, &disabled_keys_recovery, confirm_reapply);
        self.disabled_keys_recovery = next_disabled_keys_recovery;
        let followup = std::mem::take(&mut self.reapply_followup);
        let (targets, next_followup) =
            plan_reapply(&self.devices, &devices, &followup, forced, confirm_reapply);
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
        for &idx in &targets {
            self.reapply_volatile_settings(&devices[idx], &devices);
        }
        self.drive_disabled_keys_recovery(&devices, &targets, &disabled_keys_targets);
        let changed = next_current != self.current
            || devices.len() != self.devices.len()
            || devices.iter().zip(&self.devices).any(|(a, b)| {
                a.config_key != b.config_key
                    || a.route != b.route
                    || a.capabilities != b.capabilities
                    || a.light_capabilities != b.light_capabilities
            });
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

    /// Start each planned Disable Keys recovery and announce it to clients,
    /// then close the recovery of every online keyboard that no longer needs
    /// one. A keyboard in `volatile_targets` already queued its write.
    fn drive_disabled_keys_recovery(
        &mut self,
        devices: &[AgentDevice],
        volatile_targets: &[usize],
        recovery_targets: &[usize],
    ) {
        let mut recovery_started = false;
        for &idx in recovery_targets {
            let Some(route) = &devices[idx].route else {
                continue;
            };
            if !volatile_targets.contains(&idx) {
                let _ = self.shared.reapply_disabled_keys(route);
            }
            bump_settings_epoch(&mut self.settings_epochs, route);
            recovery_started = true;
        }
        // Each recovery write above already holds its place in the keyboard's
        // queue, so a client's next read waits for it and sees the result.
        if recovery_started {
            self.publish_inventory();
        }
        for dev in devices {
            if dev.online
                && !self.disabled_keys_recovery.contains_key(&stable_id(dev))
                && let Some(route) = &dev.route
            {
                self.shared.disabled_keys_order.finish_recovery(route);
            }
        }
    }

    /// Whether device settings still need a delayed inventory confirmation.
    #[must_use]
    pub fn needs_reapply_confirmation(&self) -> bool {
        !self.reapply_followup.is_empty() || !self.disabled_keys_recovery.is_empty()
    }

    /// Show a lock key a keyboard turned on or off, unless the user turned the
    /// indicator off or this platform gives the key no meaning.
    pub fn announce_lock_key(&self, change: LockKeyChange) {
        if self
            .observable
            .lock_keys()
            .announce(change, &self.config.app_settings)
        {
            debug!(?change, "showing a lock key");
        }
    }

    /// Post a low-battery notice for each online device whose battery has just
    /// come to need attention, as [`LowBatteryWatch::take_reading`] decides
    /// under the user's settings. An offline device's reading is only its last
    /// known one, and says nothing new.
    fn announce_low_batteries(&mut self, devices: &[AgentDevice]) {
        for device in devices.iter().filter(|device| device.online) {
            let Some(battery) = device.battery.as_ref() else {
                continue;
            };
            if !self.low_battery.take_reading(
                &self.config.app_settings,
                &device.config_key,
                battery,
            ) {
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

    /// Advance only the captures whose own online transition requires it.
    /// Keep a resume request latched until an authoritative snapshot arrives.
    fn rearm_capture_routes(&mut self, devices: &[AgentDevice]) {
        let all = std::mem::take(&mut self.forced_capture_rearm);
        self.capture_route_generations
            .retain(|route, _| devices.iter().any(|dev| dev.route.as_ref() == Some(route)));
        let targets = reapply_targets(&self.devices, devices, all);
        if targets.is_empty() {
            return;
        }
        let generation = self
            .shared
            .capture_rearm_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        for idx in targets {
            if let Some(route) = &devices[idx].route {
                self.capture_route_generations
                    .insert(route.clone(), generation);
            }
        }
        debug!(generation, "device route(s) require capture re-arm");
    }

    /// Move the panel epoch of every device whose volatile settings re-apply
    /// on this snapshot — a new identity, an offline→online transition, or,
    /// when `forced`, every online device — so the key-display watcher
    /// repaints a panel that may have reset. Confirming re-applies repaint
    /// nothing: the first write already did.
    fn repaint_panels(&mut self, devices: &[AgentDevice], forced: bool) {
        self.panel_generations
            .retain(|route, _| devices.iter().any(|dev| dev.route.as_ref() == Some(route)));
        let targets = reapply_targets(&self.devices, devices, forced);
        if targets.is_empty() {
            return;
        }
        self.panel_epoch = self.panel_epoch.wrapping_add(1);
        for idx in targets {
            if let Some(route) = &devices[idx].route {
                self.panel_generations
                    .insert(route.clone(), self.panel_epoch);
            }
        }
    }

    /// Reapply settings with the full confirmation budget after a node change
    /// or recovery. Capture follows its own route and listener transitions;
    /// a general settings request must not cycle unrelated capture epochs.
    pub fn reapply_volatile_on_next_refresh(&mut self) {
        self.force_reapply(VOLATILE_REAPPLY_CONFIRM_RETRIES);
    }

    /// After system resume, reapply settings and rearm every online capture.
    /// Devices may have lost volatile diversion even when the snapshot looks
    /// identical; retain this intent until an authoritative refresh arrives.
    pub fn reapply_after_system_resume(&mut self) {
        self.forced_capture_rearm = true;
        self.reapply_volatile_on_next_refresh();
    }

    /// Force a volatile-settings re-apply on the next inventory refresh
    /// because a device reconnected unobserved: it broadcast its own
    /// reconnection (`0x1d4b`), the firmware explicitly asking to be
    /// reconfigured, or the enumerator replaced its dead channel while still
    /// replaying it as online. As after a system wake, the snapshot that
    /// follows can look identical to the last one (a nap or a link drop short
    /// enough that no reconciliation ever observed the device offline), so
    /// the offline→online trigger cannot be relied on.
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
    ///
    /// `devices` is the inventory that `dev` comes from. The cookie plan reads
    /// it, not `self.devices`, which still holds the previous inventory while
    /// [`Self::refresh_inventory`] runs: there `dev` and the keyboards that
    /// came online with it would still look offline.
    fn reapply_volatile_settings(&self, dev: &AgentDevice, devices: &[AgentDevice]) {
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
        let saved = saved_speed(&self.config, dev);
        let speed_write =
            saved.and_then(|saved| self.shared.pointer_speeds.reapply(key, &route, saved));
        // A DPI mouse gets its DPI from `speed_write`, numbered with the
        // precision presses and the settings-app writes, so a newer one
        // supersedes it. The volatile reapply leaves the DPI out then.
        let dpi_overridden = matches!(saved, Some(SavedSpeed::Dpi(_))) && speed_write.is_some();
        let settings = VolatileMouseSettings {
            wheel: self.wheel_mode_change(dev),
            dpi: device
                .and_then(|d| d.effective_dpi(&route_key))
                .filter(|_| !dpi_overridden),
            report_rate: configured_report_rate(&self.config, dev),
            smartshift: device
                .and_then(|d| d.effective_smartshift(&route_key))
                .map(openlogi_hid::SmartShiftStatus::from),
        };
        // The saved DPI goes out with the speed write on a DPI mouse.
        let written_dpi = settings
            .dpi
            .or_else(|| speed_write.as_ref().and_then(SpeedWrite::dpi));
        if let Ok(mut cycles) = self.shared.dpi_cycle.write() {
            cycles.reconnected(key);
            if let Some(dpi) = written_dpi {
                cycles.reapplied(key, dpi);
            }
        }
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
        if let Some(force) = configured_force_sensing(&self.config, dev) {
            self.shared.write_force_sensing_in_background(&route, force);
        }
        if let Some(write) = speed_write {
            self.write_pointer_speed(write);
        }
        self.keep_os_layout(dev);
        let _ = self.shared.reapply_disabled_keys(&route);
        if let Some(capabilities) = dev.light_capabilities
            && let Some(light) = self.effective_light_settings(key)
        {
            self.shared
                .hardware
                .set_light_in_background(Some(route), &light, capabilities);
        }
        let writes =
            reconnect_follower_cookies(&self.config, devices, dev, &mut self.follower_cookies());
        self.write_follower_cookies(writes);
    }

    /// Write the `ChangeHost` cookies of linked pointing devices, so that Logi
    /// Options+ on a keyboard's other hosts moves the same devices.
    fn write_follower_cookies(&self, writes: Vec<FollowerCookieWrite>) {
        for write in writes {
            let state = Arc::clone(&self.follower_cookies);
            let key = write.key;
            crate::hardware::write_follower_cookies_in_background(
                self.shared.device(&write.follower),
                self.shared.follower_cookie_order.request(&write.follower),
                write.release,
                write.link,
                move |leads| {
                    state
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .requeue(&key, leads);
                },
            );
        }
    }

    /// The held cookie clears and the known keyboard bytes. A panic while the
    /// state was held leaves at worst a clear too many or too few.
    fn follower_cookies(&self) -> MutexGuard<'_, FollowerCookieState> {
        self.follower_cookies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

    /// Ask clients to re-read settings after an agent-owned hardware attempt.
    ///
    /// An error can follow a successful device write, so this announces
    /// invalidation, not success or a returned value. A disappeared route
    /// stays absent; a replacement route can only publish its own fresh read.
    /// This does not rescan inventory or re-apply persisted settings.
    pub fn announce_settings_changed(&mut self, route: &DeviceRoute) {
        let InventoryState::Ready { inventories, .. } = &self.inventory else {
            return;
        };
        if inventories.iter().any(|inventory| {
            inventory.paired.iter().any(|device| {
                device.online
                    && DeviceRoute::for_slot(inventory, device.slot).as_ref() == Some(route)
            })
        }) {
            bump_settings_epoch(&mut self.settings_epochs, route);
            self.publish_inventory();
        }
    }

    /// Republish the device facts the GUI observes, reading the one field that
    /// holds them so this can never disagree with the `inventory` /
    /// `standalone` / `inventory_health` accessors. Called by every mutator
    /// that touches the `inventory` field, and after an agent-owned settings
    /// change. It clones to add settings epochs, and otherwise only when
    /// something actually changed.
    fn publish_inventory(&self) {
        let health = self.inventory_health();
        match &self.inventory {
            InventoryState::Ready {
                inventories,
                standalone,
            } => {
                let inventories = published_inventories(inventories, &self.settings_epochs);
                self.observable.set_inventory(
                    health,
                    &inventories,
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
        openlogi_inject::set_desktop_switch(desktop_switch(&self.config.app_settings));
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
        self.apply_changed_force_sensing(&previous);
        for dev in &self.devices {
            let was_enabled = dev
                .resolve_config_key(&previous)
                .is_some_and(|key| previous.device_enabled(key.as_str()));
            let enabled = dev
                .resolve_config_key(&self.config)
                .is_some_and(|key| self.config.device_enabled(key.as_str()));
            if was_enabled
                && !enabled
                && let Some(route) = &dev.route
            {
                self.shared.disabled_keys_order.retire(route);
            }
            if self.sync_disabled_keys_policy(dev)
                && dev.online
                && let Some(route) = &dev.route
            {
                let _ = self.shared.reapply_disabled_keys(route);
            }
        }

        self.apply_changed_pointer_speeds(&previous);
        self.apply_enabled_os_layouts(&previous);
        let writes = changed_follower_cookies(
            &previous,
            &self.config,
            &self.devices,
            &mut self.follower_cookies(),
        );
        self.write_follower_cookies(writes);
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

    fn sync_disabled_keys_policy(&self, dev: &AgentDevice) -> bool {
        let Some(route) = &dev.route else {
            return false;
        };
        self.shared.disabled_keys_order.sync_policy(
            route,
            dev.resolve_config_key(&self.config)
                .and_then(|key| configured_disabled_keys(&self.config, key.as_str())),
        )
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

    /// Push a changed press force to the online, enabled force-sensing
    /// device it belongs to — [`changed_force_sensing`] picks the writes. The
    /// GUI also writes directly through [`SharedHandles::set_force_sensing`];
    /// both writes are ordered per device, so the newest request is the one
    /// that stays.
    fn apply_changed_force_sensing(&self, previous: &Config) {
        for (route, force) in changed_force_sensing(previous, &self.config, &self.devices) {
            self.shared.write_force_sensing_in_background(&route, force);
        }
    }

    /// End precision mode on every mouse whose speed, DPI or button
    /// assignments the reload changed, online or not, and push the resulting
    /// speed to the online ones (see [`crate::pointer_speed`]). An unrelated
    /// save writes nothing, as for [`Self::apply_changed_fn_locks`].
    fn apply_changed_pointer_speeds(&self, previous: &Config) {
        // A DPI is saved per link, so it is compared for the link each known
        // mouse is on.
        let dpi_changed: HashSet<&str> = self
            .devices
            .iter()
            .filter(|dev| configured_dpi(previous, dev) != configured_dpi(&self.config, dev))
            .map(|dev| dev.config_key.as_str())
            .collect();
        let change = |key: &str| ReloadChange {
            speed: self.config.pointer_speed(key) != previous.pointer_speed(key)
                || dpi_changed.contains(key),
            assignments: assignments_changed(previous, &self.config, key),
        };
        for write in self.shared.pointer_speeds.reload(change, self.speed_mice()) {
            self.write_pointer_speed(write);
        }
    }

    fn write_pointer_speed(&self, write: SpeedWrite) {
        let SpeedWrite {
            route,
            target,
            ticket,
            restore,
            led,
        } = write;
        let speeds = Arc::clone(&self.shared.pointer_speeds);
        write_pointer_speed_in_background(
            self.shared.device(&route),
            ticket,
            target,
            led,
            None,
            move || {
                if let Some(restore) = restore {
                    speeds.restored(&restore);
                }
            },
            || {},
        );
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

/// How Previous and Next Desktop switch under `settings`.
fn desktop_switch(settings: &AppSettings) -> openlogi_inject::DesktopSwitch {
    if settings.animate_desktop_switch {
        openlogi_inject::DesktopSwitch::Animated
    } else {
        openlogi_inject::DesktopSwitch::Instant
    }
}

fn configured_disabled_keys(config: &Config, device_key: &str) -> Option<DisableKeysMask> {
    if !config.device_enabled(device_key) {
        return None;
    }
    config.disabled_keys(device_key).map(|keys| {
        keys.iter()
            .fold(DisableKeysMask::EMPTY, |mask, key| mask | key.mask())
    })
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
/// route, and at least one of its keys is bound in some profile. `divert` is
/// what the app in front binds, and may be empty.
struct KeyboardCandidate<'a> {
    dev: &'a AgentDevice,
    bindings: BTreeMap<ButtonId, Binding>,
    divert: KeyboardDivertSet,
}

/// What the bindings ask a keyboard capture session to do: the `0x1b04`
/// controls split into the ones the session will arm and the reserved ones it
/// refuses, plus the keypad grid keys it will only ever observe.
#[derive(Debug, Default, PartialEq, Eq)]
struct KeyboardDivertSet {
    wanted: BTreeMap<u16, ButtonId>,
    reserved: Vec<u16>,
    grid: BTreeMap<u8, ButtonId>,
}

impl KeyboardDivertSet {
    /// Whether the session would capture no key at all.
    fn captures_nothing(&self) -> bool {
        self.wanted.is_empty() && self.grid.is_empty()
    }
}

/// Whether a keyboard control or keypad grid key carries a real action
/// ([`Binding::key_action`]). The capture set and a keypad key's face both
/// decide "does this key do anything" there.
fn is_bound_key(binding: &Binding) -> bool {
    binding.key_action().is_some()
}

/// What a keypad grid key shows for `binding`: blank exactly when the key does
/// nothing, else the glyph of [`Binding::key_action`].
fn key_face(binding: Option<&Binding>) -> KeyFace {
    binding
        .and_then(Binding::key_action)
        .map_or(KeyFace::Blank, |action| {
            KeyFace::Icon(ActionRingIcon::for_action(&action))
        })
}

/// Which of `bindings` the keyboard capture session takes over.
///
/// A control is diverted whenever it is stored at all, even as
/// [`Action::None`](openlogi_core::binding::Action::None) (which means
/// "suppress this key," not "leave native"):
/// `button_bindings_for` seeds no default for a [`ButtonId::Control`], so
/// presence here is always an explicit user choice (#1747). A keypad grid key
/// is only observed — the session never diverts it, so it cannot suppress the
/// key — and joins the set only when it carries a real action
/// ([`is_bound_key`]).
fn keyboard_divert_set(bindings: &BTreeMap<ButtonId, Binding>) -> KeyboardDivertSet {
    let mut set = KeyboardDivertSet::default();
    for (button, binding) in bindings {
        // A grid key's identity is its position, and the session observes it
        // without writing anything. Bound on any page, the physical key joins
        // the observed set, so a page turn — which only changes what a press
        // dispatches — never changes the capture identity and never restarts
        // the session.
        if let Some(slot) = button.grid_slot() {
            if is_bound_key(binding) {
                set.grid.insert(slot.key.index(), ButtonId::grid(slot.key));
            }
            continue;
        }
        let Some(cid) = button.cid().map(openlogi_core::binding::Cid::raw) else {
            continue;
        };
        if is_reserved_keyboard_control(cid) {
            set.reserved.push(cid);
        } else {
            set.wanted.insert(cid, *button);
        }
    }
    set
}

/// Whether `config_key` binds a key the capture session would take over in
/// its device-wide profile or in any of its per-app profiles. The
/// device-wide profile is read with the defaults the session dispatches, so
/// a keypad with more than one page captures its paging keys even with
/// nothing else bound. A per-app override replaces the whole button with a
/// single action, so its presence captures the key exactly as the same
/// device-wide binding does.
fn binds_keys_in_some_profile(config: &Config, config_key: &str) -> bool {
    let device_wide = button_bindings_for(config, Some(config_key), None, None);
    !keyboard_divert_set(&device_wide).captures_nothing()
        || config
            .app_profiles(config_key)
            .filter_map(|app| config.per_app_overrides(config_key, app))
            .any(|overrides| {
                let overrides = overrides
                    .iter()
                    .map(|(button, action)| (*button, Binding::Single(action.clone())))
                    .collect();
                !keyboard_divert_set(&overrides).captures_nothing()
            })
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

#[cfg(test)]
mod tests;
