//! App-wide UI state stored as a GPUI global.
//!
//! Anything that more than one view needs to read (current device, currently
//! armed button, the DPI value the panel and the dot-preview share) lives
//! here. Per-component scratch state (hover index) stays
//! in the owning entity.
//!
//! [`AppState::with_runtime`] resolves every paired device's asset + DPI
//! target up front so views can switch instantly when the carousel selection
//! changes — no synchronous I/O during the device switch.

use std::collections::BTreeMap;

use gpui::Global;
use openlogi_core::config::{Config, KeyTrigger, LightSettings};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_hid::{DpiInfo, SmartShiftStatus};
use tokio::sync::mpsc;
use tracing::warn;

pub use devices::DeviceRecord;
pub use light::LightCommandStatus;
#[cfg(test)]
pub use load::Load;
pub use load::{DpiStatus, SmartShiftLoad};

/// Result of confirming a SmartShift write by reading the value back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartShiftWriteStatus {
    /// The optimistic value is visible while the confirming read runs.
    Applying {
        /// Value written optimistically.
        expected: SmartShiftStatus,
        /// Identity used to reject replies from older writes.
        write_id: u64,
    },
    /// The device returned the value that was written.
    Confirmed,
    /// The confirming read failed, closed, or returned a different value.
    Failed,
}

pub(crate) use devices::camera_model_info;
use light::PendingLightCommand;
use load::LazyDeviceData;

use crate::asset::AssetResolver;
use crate::data::mouse_buttons::{Action, ButtonId, GestureDirection};
use crate::state::devices::{build_device_list, pick_initial_device};
use openlogi_core::binding::{ActionRingConfig, ActionRingIcon, ActionRingSlot, RingAction};

mod agent;
mod bindings;
mod camera;
mod devices;
mod dpi;
mod inventory;
mod light;
mod lighting;
mod load;
mod scroll;
mod settings;
mod smartshift;

#[cfg(test)]
mod tests;

/// Default DPI value applied to a fresh AppState. Matches a common Logitech
/// mid-range mouse and keeps the dot-preview visually obvious from frame one.
pub const DEFAULT_DPI: u32 = 1600;

/// The GUI's view of the agent connection: the latest status snapshot, or the
/// reason there isn't one. One value instead of per-fact mirror fields
/// (granted / scanning / …) so a future writer can't update half of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLink {
    /// No snapshot yet — the window just opened, or the agent is still
    /// starting. Render a neutral connecting frame: claiming "denied" or "no
    /// devices" before the first snapshot flashed both at every
    /// already-set-up user (the original startup bug).
    Connecting,
    /// Still no snapshot well past startup: the agent is genuinely
    /// unreachable (binary missing, repeated spawn failures). Rendered as a
    /// static error frame; polling continues and a snapshot upgrades this
    /// back to [`Self::Ready`].
    Unreachable,
    /// The agent answered the handshake with a *newer* protocol than this
    /// process speaks — the app was updated on disk while this GUI stayed
    /// running. Only relaunching helps; without this state the window would
    /// keep showing a live-looking but frozen UI.
    OutdatedGui,
    /// Connected and current: the agent's latest status snapshot.
    Ready(openlogi_agent_core::ipc::AgentStatus),
}

/// Where [`AppState`] may persist configuration mutations.
///
/// Runtime state uses [`Self::UserFile`]. Tests opt into
/// [`Self::MemoryOnly`] so realistic device fixtures can never modify the
/// developer's actual `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPersistence {
    /// Persist to OpenLogi's default per-user configuration file.
    UserFile,
    /// Keep changes in the in-memory [`Config`] only.
    MemoryOnly,
}

/// Inventory snapshots can briefly miss a real device while another HID++
/// request is in flight. Keep the previous record through this many
/// consecutive misses so a transient probe timeout does not make the carousel
/// disappear mid-interaction.
const INVENTORY_MISS_GRACE: u8 = 2;

pub struct AppState {
    /// Index into [`Self::device_list`] of the currently visible device. May
    /// be out of bounds briefly while inventories re-enumerate; views must
    /// bounds-check via [`Self::current_record`].
    pub current_device: usize,
    /// Bundle identifier of the frontmost macOS app (P1.4), or `None` on
    /// non-macOS / no frontmost app. Used to overlay per-app bindings on
    /// top of the per-device global map.
    pub current_app_bundle: Option<String>,
    /// Aggregate host-camera activity reported by the agent. Runtime only.
    camera_active: bool,
    /// Transient manual power choices for camera-linked lights. Cleared on
    /// the next camera-state transition and never persisted as an override.
    manual_light_overrides: BTreeMap<String, bool>,
    /// Session-only settings for raw devices whose OS-node identity is not
    /// stable enough to persist in `config.toml`.
    volatile_light_settings: BTreeMap<String, LightSettings>,
    light_commands: BTreeMap<String, PendingLightCommand>,
    light_command_status: Option<(String, u64, LightCommandStatus)>,
    next_light_request_id: u64,
    /// The hotspot the user most recently armed by clicking. Drives the
    /// "selected button" outline on the mouse model and the popover content.
    pub active_button: Option<ButtonId>,
    /// Everything the GUI knows about the agent connection — the last status
    /// snapshot, or why there isn't one. The render path branches on this
    /// single value, so the permission gate, the scanning state, and the
    /// connection-problem frames can never disagree about what the agent said.
    agent_link: AgentLink,
    /// Bindings for the *currently selected* device. Reloaded whenever the
    /// carousel selection changes.
    pub button_bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction sub-bindings for the current device's gesture owner. Edited
    /// via the gesture picker and persisted as a [`Binding::Gesture`] entry under
    /// the owning button — the HID++ gesture button ([`ButtonId::GestureButton`]) by default,
    /// or a promoted Middle/Back/Forward — in the device's unified binding map
    /// ([`DeviceConfig::bindings`]). Rebuilt by the `gesture_bindings_for_current` helper.
    ///
    /// [`DeviceConfig::bindings`]: openlogi_core::config::DeviceConfig::bindings
    pub gesture_bindings: BTreeMap<GestureDirection, Action>,
    /// Global keyboard F-key bindings (Esc + F1-F19). Device-agnostic — one
    /// map applies across all keyboards — so, unlike [`Self::button_bindings`],
    /// this is *not* reloaded on device switch. Seeded once from
    /// [`Config::keyboard`] and kept in sync via [`Self::commit_keyboard_binding`].
    /// Sorted (`BTreeMap`) for stable render order in the function-row view.
    pub keyboard_bindings: BTreeMap<KeyTrigger, Action>,
    pub dpi: u32,
    /// DPI capability load state keyed by [`DeviceRecord::config_key`]. Loaded
    /// lazily because HID++ reads must not block device switching or rendering.
    dpi_data: LazyDeviceData<DpiInfo>,
    /// Consecutive inventory snapshots that omitted a previously-known device,
    /// keyed by [`DeviceRecord::config_key`]. Used to debounce transient HID++
    /// probe misses without hiding a real disconnect forever.
    inventory_misses: BTreeMap<String, u8>,
    /// SmartShift (`0x2111`) config load state keyed by
    /// [`DeviceRecord::config_key`]. Loaded lazily on the same pattern as
    /// [`Self::dpi_data`]; the device persists the values itself, so this is a
    /// read/write cache, not a source of truth saved to disk.
    smartshift_data: LazyDeviceData<SmartShiftStatus>,
    /// Devices whose SmartShift was just written optimistically and still need a
    /// confirming re-read, keyed by [`DeviceRecord::config_key`]. A fire-and-
    /// forget write can be rejected/timed-out by a sleeping device, so the panel
    /// re-reads (without a Loading flicker) to replace the optimistic value with
    /// the device's actual state. See [`Self::commit_smartshift`].
    smartshift_pending_confirm: BTreeMap<String, u64>,
    /// Monotonic identity assigned to the next confirmable SmartShift write.
    next_smartshift_write_id: u64,
    /// Visible outcome of the post-write SmartShift confirmation.
    smartshift_write_status: BTreeMap<String, SmartShiftWriteStatus>,
    /// All paired devices, in carousel order. Each entry caches the per-
    /// device data the views need so a switch is a pure index update.
    pub device_list: Vec<DeviceRecord>,
    /// Live config — kept in sync with disk via [`Self::commit_binding`] and
    /// [`Self::set_current_device`] so restarts preserve user bindings and
    /// the last-selected device.
    config: Config,
    /// Sender to the IPC client thread. The agent owns the hook + all device
    /// I/O, so binding / setting writes persist to `config.toml` and then send
    /// [`Command::ReloadConfig`](crate::ipc_client::Command) for the agent to
    /// rebuild, and "apply now" device changes (DPI / SmartShift / lighting)
    /// go out as their own commands. The GUI never opens a device itself.
    ipc_commands: mpsc::UnboundedSender<crate::ipc_client::Command>,
    /// Explicit persistence boundary; tests use an in-memory-only state.
    config_persistence: ConfigPersistence,
    /// Raw inventory from the last *completed* enumeration, kept for the
    /// diagnostics report (receivers + transports). The poll path only stores
    /// [`InventoryHealth::Ready`](openlogi_agent_core::ipc::InventoryHealth)
    /// snapshots, so an agent restart's empty pre-enumeration list never
    /// blanks a report copied during the reconnect window.
    last_inventory: Vec<DeviceInventory>,
    /// Recent events streamed from the agent's hook for the debug live monitor
    /// on the Diagnostics page. Bounded; only filled while the Settings window's
    /// poll loop runs (debug macOS builds only).
    #[cfg(all(target_os = "macos", debug_assertions))]
    monitor_events: std::collections::VecDeque<openlogi_agent_core::ipc::MonitorEvent>,
    /// Cached event-tap snapshot for the Diagnostics page, refreshed on the same
    /// ~300ms tick as [`Self::monitor_events`]. Lets that page's per-frame render
    /// read this cache instead of issuing `CGGetEventTapList` syscalls on every
    /// repaint. Debug-only: the release Diagnostics page enumerates taps live,
    /// since it renders on interaction rather than on a 300ms monitor cadence.
    #[cfg(all(target_os = "macos", debug_assertions))]
    event_taps: Vec<openlogi_hook::EventTapInfo>,
}

impl AppState {
    /// Build the global from a loaded config + enumerated inventories.
    ///
    /// The initial selection prefers [`Config::selected_device`] if it still
    /// matches one of the paired devices; otherwise it falls back to index 0.
    #[must_use]
    pub fn with_runtime(
        mut config: Config,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        cache: &AssetResolver,
        cameras: &[openlogi_camera::Camera],
        config_persistence: ConfigPersistence,
        ipc_commands: mpsc::UnboundedSender<crate::ipc_client::Command>,
    ) -> Self {
        let device_list = build_device_list(inventories, standalone, cache, &config, cameras);
        // Record any device probed at launch so it survives the next cold start.
        let identities_changed = inventory::persist_identities(&mut config, &device_list);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let mut state = Self {
            current_device,
            current_app_bundle: None,
            camera_active: false,
            manual_light_overrides: BTreeMap::new(),
            volatile_light_settings: BTreeMap::new(),
            light_commands: BTreeMap::new(),
            light_command_status: None,
            next_light_request_id: 0,
            active_button: None,
            // Updated from the agent's IPC poll; the GUI no longer runs the
            // hook, so it can't meaningfully query Accessibility (or devices)
            // itself.
            agent_link: AgentLink::Connecting,
            button_bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            keyboard_bindings: BTreeMap::new(),
            dpi: DEFAULT_DPI,
            dpi_data: LazyDeviceData::default(),
            inventory_misses: BTreeMap::new(),
            smartshift_data: LazyDeviceData::default(),
            smartshift_pending_confirm: BTreeMap::new(),
            next_smartshift_write_id: 0,
            smartshift_write_status: BTreeMap::new(),
            device_list,
            config,
            ipc_commands,
            config_persistence,
            last_inventory: Vec::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            monitor_events: std::collections::VecDeque::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            event_taps: Vec::new(),
        };
        if identities_changed {
            state.persist_config("device identity");
        }
        state.button_bindings = state.bindings_for_current();
        state.gesture_bindings = state.gesture_bindings_for_current();
        // Keyboard bindings are global, so they seed straight from the config
        // map — no per-device resolution like mouse bindings above.
        state.keyboard_bindings = state
            .config
            .keyboard
            .bindings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        state
    }
    /// Send a device command to the agent over IPC, logging a dropped channel
    /// (the client thread is gone) rather than surfacing it.
    fn send_ipc(&self, command: crate::ipc_client::Command) -> bool {
        if self.ipc_commands.send(command).is_err() {
            warn!("IPC client thread is gone — device command dropped");
            return false;
        }
        true
    }
    /// Persist the in-memory config and — only if the write actually landed —
    /// have the agent reload it. `what` names the setting for the failure log.
    ///
    /// The order matters: on a failed write the on-disk file still holds the
    /// *previous* config, so a reload would hand the agent stale values and
    /// (for volatile settings) silently re-apply the old DPI/SmartShift on the
    /// next reconnect or wake. Skipping the reload keeps the agent on whatever
    /// it already runs; the GUI keeps the new value in memory either way.
    fn persist_and_reload(&self, what: &str) {
        if self.persist_config(what) {
            self.send_ipc(crate::ipc_client::Command::ReloadConfig);
        }
    }
    fn persist_config(&self, what: &str) -> bool {
        if self.config_persistence == ConfigPersistence::MemoryOnly {
            return true;
        }
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, what, "could not persist to config.toml");
            return false;
        }
        true
    }
    /// A clone of the IPC command sender, so views (the DPI / SmartShift panels)
    /// can issue device reads and writes through the agent themselves.
    #[must_use]
    pub fn ipc_sender(&self) -> mpsc::UnboundedSender<crate::ipc_client::Command> {
        self.ipc_commands.clone()
    }
    /// Cache a *completed* inventory snapshot for the diagnostics report.
    /// Callers gate on [`InventoryHealth::Ready`](openlogi_agent_core::ipc::InventoryHealth) —
    /// see [`Self::last_inventory`].
    pub fn store_inventory_snapshot(&mut self, inventory: &[DeviceInventory]) {
        self.last_inventory = inventory.to_vec();
    }
    /// The last completed inventory snapshot, used by diagnostics for transports and receivers.
    #[must_use]
    pub fn last_inventory(&self) -> &[DeviceInventory] {
        &self.last_inventory
    }
    /// Config schema version and the number of devices with saved configuration.
    #[must_use]
    pub fn config_summary(&self) -> (u32, usize) {
        (self.config.schema_version, self.config.devices.len())
    }
    /// The active device, or `None` when [`Self::device_list`] is empty or
    /// `current_device` is past the end.
    #[must_use]
    pub fn current_record(&self) -> Option<&DeviceRecord> {
        self.device_list.get(self.current_device)
    }

    /// Actions Ring settings for the active device, including its implicit
    /// default layout when nothing has been persisted yet.
    #[must_use]
    pub fn current_action_ring(&self) -> ActionRingConfig {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(|key| self.config.action_ring(key))
            .unwrap_or_default()
    }

    /// Replace or clear one slot in the active device's default ring layout.
    pub fn commit_action_ring_slot(&mut self, slot: ActionRingSlot, action: Option<RingAction>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config.set_action_ring_slot(&key, slot, action);
        self.persist_and_reload("Actions Ring slot");
    }

    /// Set or restore the action-derived icon for one active-device ring slot.
    pub fn commit_action_ring_icon(&mut self, slot: ActionRingSlot, icon: Option<ActionRingIcon>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config.set_action_ring_icon(&key, slot, icon);
        self.persist_and_reload("Actions Ring icon");
    }

    /// Enable or disable the active device's Actions Ring.
    pub fn commit_action_ring_enabled(&mut self, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config.set_action_ring_enabled(&key, enabled);
        self.persist_and_reload("Actions Ring enabled state");
    }

    /// Enable or disable hover and activation haptics for the active ring.
    pub fn commit_action_ring_haptics(&mut self, enabled: bool) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        self.config.set_action_ring_haptics(&key, enabled);
        self.persist_and_reload("Actions Ring haptics");
    }
}

impl Global for AppState {}
