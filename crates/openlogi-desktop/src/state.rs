//! App-wide UI state owned by a GPUI entity.
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

use gpui::{App, Context, Entity, EventEmitter, Global};
use openlogi_core::app::ForegroundApp;
use openlogi_core::binding::{
    Action, ActionRingConfig, ActionRingIcon, ActionRingSlot, ButtonId, GestureDirection,
    RingAction,
};
use openlogi_core::config::{Config, ConfigFile, KeyTrigger};
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::hid::{Dpi, SmartShiftStatus};
use openlogi_ipc::ForegroundApps;
use tokio::sync::mpsc;
use tracing::warn;

pub(crate) use device_key::DeviceKey;
pub use devices::DeviceRecord;
pub use light::LightCommandStatus;
pub(crate) use load::Load;
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

use device_ui::DeviceUiState;
pub(crate) use devices::camera_model_info;

use crate::services::assets::AssetResolver;
use crate::services::device_reads::DeviceReads;
use crate::state::devices::{build_device_list, pick_initial_device};

mod agent;
mod bindings;
mod camera;
mod device_key;
mod device_ui;
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
pub const DEFAULT_DPI: Dpi = Dpi::new(1600);

/// Semantic changes emitted by the shared application-state entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StateEvent {
    /// Agent connection or permission state changed.
    AgentChanged,
    /// The foreground application or recent-application list changed.
    ForegroundChanged,
    /// Cached diagnostics/event-monitor data changed.
    #[cfg_attr(
        not(all(target_os = "macos", debug_assertions)),
        expect(dead_code, reason = "the live event monitor is macOS debug-only")
    )]
    DiagnosticsChanged,
    /// The merged device inventory changed.
    InventoryChanged,
    /// The active carousel device changed.
    DeviceSelected(DeviceKey),
    /// Mouse, keyboard, gesture, or Actions Ring bindings changed.
    BindingsChanged(DeviceKey),
    /// DPI data or the active DPI value changed.
    DpiChanged(DeviceKey),
    /// SmartShift data or write status changed.
    SmartShiftChanged(DeviceKey),
    /// Device or standalone-light settings changed.
    LightingChanged(DeviceKey),
    /// Camera settings or activity changed.
    CameraChanged,
    /// Host camera-permission status may have changed.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "camera consent polling is macOS-only")
    )]
    CameraPermissionChanged,
    /// Per-device preferences outside the feature-specific events changed.
    DeviceConfigChanged(DeviceKey),
    /// Application-wide preferences changed.
    SettingsChanged,
}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

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
    Ready(openlogi_ipc::AgentStatus),
}

/// Where [`AppState`] may persist configuration mutations.
///
/// Runtime state uses [`Self::UserFile`]. Tests opt into
/// [`Self::MemoryOnly`] so realistic device fixtures can never modify the
/// developer's actual `config.toml`.
#[derive(Debug, Clone)]
pub enum ConfigPersistence {
    /// Persist through the tracked user file, preserving comments and refusing
    /// to overwrite edits made after startup.
    UserFile(ConfigFile),
    /// A load error made the config unsafe to write for this process lifetime.
    ReadOnly(String),
    /// Keep changes in the in-memory [`Config`] only.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "test-only persistence boundary")
    )]
    MemoryOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigIssue {
    Persistence(String),
    Reload(String),
}

impl ConfigIssue {
    fn message(&self) -> &str {
        match self {
            Self::Persistence(message) | Self::Reload(message) => message,
        }
    }
}

/// Inventory snapshots can briefly miss a real device while another HID++
/// request is in flight. Keep the previous record through this many
/// consecutive misses so a transient probe timeout does not make the carousel
/// disappear mid-interaction.
const INVENTORY_MISS_GRACE: u8 = 2;

/// The per-app profile the binding panels are editing, and the device it was
/// chosen for.
///
/// The device is stored with it because an overlay is per-device: carrying
/// Safari's scope onto the next mouse would silently edit a profile the user
/// never opened. Pairing them makes the scope self-invalidating on a device
/// switch, rather than something every path that moves the selection — there
/// are two today — has to remember to reset.
struct EditingScope {
    device_key: String,
    app: String,
}

pub struct AppState {
    /// Index into [`Self::device_list`] of the currently visible device. May
    /// be out of bounds briefly while inventories re-enumerate; views must
    /// bounds-check via [`Self::current_record`].
    pub current_device: usize,
    /// Which application the agent is resolving per-app profiles against, and
    /// the ones it recently saw in front. Read-only: the agent owns it, and
    /// these identifiers are the only ones guaranteed to match what its matcher
    /// compares — see [`ForegroundApps`].
    foreground: ForegroundApps,
    /// The per-app profile the binding panels are editing, if not the device's
    /// global one. See [`EditingScope`].
    editing_scope: Option<EditingScope>,
    /// Aggregate host-camera activity reported by the agent. Runtime only.
    camera_active: bool,
    /// Per-device UI state outside the persisted config and the lazily-loaded
    /// DPI/SmartShift reads ([`Self::reads`]) —
    /// manual camera-light overrides, volatile light settings, in-flight
    /// light commands, inventory-miss counters, and SmartShift write/confirm
    /// bookkeeping. One row per device instead of one map per concern; see
    /// [`DeviceUiState`].
    device_ui: BTreeMap<DeviceKey, DeviceUiState>,
    light_command_status: Option<(DeviceKey, u64, LightCommandStatus)>,
    next_light_request_id: u64,
    /// The hotspot the user most recently armed by clicking. Drives the
    /// "selected button" outline on the mouse model and inspector content.
    pub active_button: Option<ButtonId>,
    /// Everything the GUI knows about the agent connection — the last status
    /// snapshot, or why there isn't one. The render path branches on this
    /// single value, so the permission gate, the scanning state, and the
    /// connection-problem frames can never disagree about what the agent said.
    agent_link: AgentLink,
    /// Bindings for the *currently selected* device. Reloaded whenever the
    /// carousel selection changes.
    pub button_bindings: BTreeMap<ButtonId, Action>,
    /// Cached per-direction sub-bindings for every gesture-mode button of the
    /// current device's global profile, keyed by button. The cache remains
    /// populated while editing a per-app profile so the inspector can describe
    /// inherited gestures without rebuilding the maps during rendering. Edited
    /// via each button's gesture menu and persisted as that button's
    /// [`Binding::Gesture`] entry in the device's unified binding map
    /// ([`DeviceConfig::bindings`]).
    ///
    /// [`DeviceConfig::bindings`]: openlogi_core::config::DeviceConfig::bindings
    pub gesture_bindings: BTreeMap<ButtonId, BTreeMap<GestureDirection, Action>>,
    /// Global keyboard F-key bindings (Esc + F1-F19). Device-agnostic — one
    /// map applies across all keyboards — so, unlike [`Self::button_bindings`],
    /// this is *not* reloaded on device switch. Seeded once from
    /// [`Config::keyboard`] and kept in sync via [`Self::commit_keyboard_binding`].
    /// Sorted (`BTreeMap`) for stable render order in the function-row view.
    pub keyboard_bindings: BTreeMap<KeyTrigger, Action>,
    pub dpi: Dpi,
    /// SWR-backed DPI and SmartShift reads keyed by [`DeviceKey`]. HID++ reads
    /// must not block device switching or rendering; feature modules project
    /// this service into synchronous [`Load`] values for their render paths.
    pub(crate) reads: DeviceReads,
    /// Monotonic identity assigned to the next confirmable SmartShift write.
    next_smartshift_write_id: u64,
    /// All paired devices, in carousel order. Each entry caches the per-
    /// device data the views need so a switch is a pure index update.
    pub device_list: Vec<DeviceRecord>,
    /// Live config — kept in sync with disk via [`Self::commit_binding`] and
    /// [`Self::set_current_device`] so restarts preserve user bindings and
    /// the last-selected device.
    config: Config,
    /// Last config revision that reached disk. Restored when a save fails so
    /// the UI cannot continue presenting an unsaved value as committed.
    persisted_config: Config,
    /// Sender to the IPC client thread. The agent owns the hook + all device
    /// I/O, so binding / setting writes persist to `config.toml` and then send
    /// [`Command::ReloadConfig`](crate::services::ipc::Command) for the agent to
    /// rebuild, and "apply now" device changes (DPI / SmartShift / lighting)
    /// go out as their own commands. The GUI never opens a device itself.
    ipc_commands: mpsc::UnboundedSender<crate::services::ipc::Command>,
    /// Explicit persistence boundary; tests use an in-memory-only state.
    config_persistence: ConfigPersistence,
    /// User-visible load, save, conflict, or agent-reload failure.
    config_issue: Option<ConfigIssue>,
    /// Raw inventory from the last *completed* enumeration, kept for the
    /// diagnostics report (receivers + transports). The poll path only stores
    /// [`InventoryHealth::Ready`](openlogi_ipc::InventoryHealth)
    /// snapshots, so an agent restart's empty pre-enumeration list never
    /// blanks a report copied during the reconnect window.
    last_inventory: Vec<DeviceInventory>,
    /// Recent events streamed from the agent's hook for the debug live monitor
    /// on the Diagnostics page. Bounded; only filled while the Settings window's
    /// poll loop runs (debug macOS builds only).
    #[cfg(all(target_os = "macos", debug_assertions))]
    monitor_events: std::collections::VecDeque<openlogi_ipc::MonitorEvent>,
    /// Cached event-tap snapshot for the Diagnostics page, refreshed on the same
    /// ~300ms tick as [`Self::monitor_events`]. Lets that page's per-frame render
    /// read this cache instead of issuing `CGGetEventTapList` syscalls on every
    /// repaint. Debug-only: the release Diagnostics page enumerates taps live,
    /// since it renders on interaction rather than on a 300ms monitor cadence.
    #[cfg(all(target_os = "macos", debug_assertions))]
    event_taps: Vec<openlogi_hook::EventTapInfo>,
}

impl AppState {
    /// Return the shared state entity when runtime initialization has installed it.
    pub(crate) fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAppState>()
            .map(|state| state.0.clone())
    }

    /// Return the shared state entity.
    #[track_caller]
    pub(crate) fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAppState>().0.clone()
    }

    /// Borrow the shared state when runtime initialization has installed it.
    pub(crate) fn try_read(cx: &App) -> Option<&Self> {
        cx.try_global::<GlobalAppState>()
            .map(|state| state.0.read(cx))
    }

    /// Update the shared state with its entity context.
    pub(crate) fn update<R>(
        cx: &mut App,
        update: impl FnOnce(&mut Self, &mut Context<Self>) -> R,
    ) -> R {
        Self::global(cx).update(cx, update)
    }

    /// Start any pending DPI/SmartShift read for the selected device. Called
    /// after inventory or selection changes; render paths only consume caches.
    pub(crate) fn load_current_device_reads(cx: &mut App) {
        Self::update(cx, |state, cx| {
            state.load_current_dpi(cx);
            state.load_current_smartshift(cx);
            state.confirm_current_smartshift(cx);
        });
    }

    /// Install the shared state entity behind its private global handle.
    pub(crate) fn set_global(state: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAppState(state));
    }

    /// Build the state from a loaded config + enumerated inventories.
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
        ipc_commands: mpsc::UnboundedSender<crate::services::ipc::Command>,
    ) -> Self {
        let persisted_config = config.clone();
        let config_issue = match &config_persistence {
            ConfigPersistence::ReadOnly(error) => Some(ConfigIssue::Persistence(error.clone())),
            ConfigPersistence::UserFile(_) | ConfigPersistence::MemoryOnly => None,
        };
        let device_list = build_device_list(inventories, standalone, cache, &config, cameras);
        // Record any device probed at launch so it survives the next cold start.
        let identities_changed = inventory::persist_identities(&mut config, &device_list);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let mut state = Self {
            current_device,
            foreground: ForegroundApps::default(),
            editing_scope: None,
            camera_active: false,
            device_ui: BTreeMap::new(),
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
            reads: DeviceReads::default(),
            next_smartshift_write_id: 0,
            device_list,
            config,
            persisted_config,
            ipc_commands,
            config_persistence,
            config_issue,
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
        state.gesture_bindings = state.device_gesture_maps();
        // Keyboard bindings are global, so they seed straight from the config
        // map — no per-device resolution like mouse bindings above.
        state.keyboard_bindings = state
            .config
            .keyboard
            .bindings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if state.config_issue.is_none()
            && matches!(&state.config_persistence, ConfigPersistence::UserFile(_))
        {
            state.send_ipc(crate::services::ipc::Command::ReloadConfig);
        }
        state
    }
    /// Send a device command to the agent over IPC, logging a dropped channel
    /// (the client thread is gone) rather than surfacing it.
    fn send_ipc(&self, command: crate::services::ipc::Command) -> bool {
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
    /// next reconnect or wake. A failed write restores the last persisted
    /// config and surfaces the persistence error in the GUI.
    fn persist_and_reload(&mut self, what: &str) -> bool {
        if self.persist_config(what) {
            self.send_ipc(crate::services::ipc::Command::ReloadConfig);
            true
        } else {
            false
        }
    }
    fn persist_config(&mut self, what: &str) -> bool {
        let result = match &mut self.config_persistence {
            ConfigPersistence::UserFile(file) => file.save(&self.config),
            ConfigPersistence::ReadOnly(_) => {
                self.restore_persisted_config();
                return false;
            }
            ConfigPersistence::MemoryOnly => Ok(()),
        };
        if let Err(error) = result {
            warn!(error = %error, what, "could not persist to config.toml");
            self.config_issue = Some(ConfigIssue::Persistence(error.to_string()));
            self.restore_persisted_config();
            return false;
        }
        self.persisted_config.clone_from(&self.config);
        if matches!(&self.config_issue, Some(ConfigIssue::Persistence(_))) {
            self.config_issue = None;
        }
        true
    }

    fn restore_persisted_config(&mut self) {
        self.config.clone_from(&self.persisted_config);
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.device_gesture_maps();
        self.keyboard_bindings = self.config.keyboard.bindings.clone();
        if let Some(dpi) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.dpi(key))
        {
            self.dpi = dpi;
        }
    }

    /// Current config failure, shown as a fail-closed whole-window notice.
    #[must_use]
    pub fn config_issue(&self) -> Option<&str> {
        self.config_issue.as_ref().map(ConfigIssue::message)
    }

    /// Record whether the agent adopted the last saved config.
    pub fn apply_config_reload_result(
        &mut self,
        result: Result<(), openlogi_ipc::ConfigReloadError>,
    ) -> bool {
        let next = match result {
            Err(error) => Some(ConfigIssue::Reload(error.message)),
            Ok(()) if matches!(&self.config_issue, Some(ConfigIssue::Reload(_))) => None,
            Ok(()) => return false,
        };
        if self.config_issue == next {
            return false;
        }
        self.config_issue = next;
        true
    }
    /// A clone of the IPC command sender used by the state entity to issue
    /// device reads and writes through the agent.
    #[must_use]
    pub fn ipc_sender(&self) -> mpsc::UnboundedSender<crate::services::ipc::Command> {
        self.ipc_commands.clone()
    }
    /// Cache a *completed* inventory snapshot for the diagnostics report.
    /// Callers gate on [`InventoryHealth::Ready`](openlogi_ipc::InventoryHealth) —
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

    /// The application whose profile the binding panels are editing, or `None`
    /// for the device's global profile.
    ///
    /// Resolves against the *current* device, so a scope chosen for another one
    /// simply does not apply — see [`EditingScope`].
    #[must_use]
    pub fn editing_app(&self) -> Option<&str> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.editing_scope
            .as_ref()
            .filter(|scope| scope.device_key == key)
            .map(|scope| scope.app.as_str())
    }

    /// Edit `app`'s profile for the active device, or its global profile with
    /// `None`. Re-derives what the panels show; nothing is persisted, because
    /// which profile is open is a property of this window, not of the config.
    pub fn set_editing_app(&mut self, app: Option<String>) {
        self.editing_scope = app
            .zip(
                self.current_record()
                    .and_then(DeviceRecord::persistent_config_key)
                    .map(str::to_string),
            )
            .map(|(app, device_key)| EditingScope { device_key, app });
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.device_gesture_maps();
    }

    /// Whether the active device can carry saved configuration at all. A
    /// transient probe — one with no stable unit id — cannot, so nothing that
    /// would write to `config.toml` for it should be offered.
    #[must_use]
    pub fn current_device_is_persistent(&self) -> bool {
        self.current_record()
            .is_some_and(DeviceRecord::is_persistent)
    }

    /// Every application profile the active device has, as
    /// `(identifier, override count)` in identifier order.
    pub fn app_profiles(&self) -> impl Iterator<Item = (&str, usize)> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .into_iter()
            .flat_map(move |key| {
                self.config.app_profiles(key).map(move |app| {
                    let count = self
                        .config
                        .per_app_overrides(key, app)
                        .map_or(0, BTreeMap::len);
                    (app, count)
                })
            })
    }

    /// Applications the agent recently saw in front, newest first, as
    /// `(identifier, display name)`. The only identifiers a picker may offer —
    /// see [`ForegroundApps`].
    pub fn recent_apps(&self) -> impl Iterator<Item = (&str, &str)> {
        self.foreground
            .recent
            .iter()
            .map(|app| (app.id.as_str(), app.display_name.as_str()))
    }

    /// The name the agent last reported for `app`, or `None` for one it has not
    /// seen this session — a hand-written profile, or one carried in from
    /// another machine.
    #[must_use]
    pub fn recent_app_name(&self, app: &str) -> Option<&str> {
        self.foreground
            .recent
            .iter()
            .find(|seen| seen.id == app)
            .map(|seen| seen.display_name.as_str())
    }

    /// Adopt the agent's view of the foreground application. Returns whether
    /// anything changed, so the caller can decide to repaint.
    pub fn set_foreground(&mut self, foreground: ForegroundApps) -> bool {
        let changed = self.foreground != foreground;
        self.foreground = foreground;
        changed
    }

    /// The application whose profile the user is asking about.
    ///
    /// Not [`ForegroundApps::current`]: while this window has focus *OpenLogi*
    /// is the frontmost application, so the app the user means is the one they
    /// came from. The recent list is exactly that — it excludes OpenLogi's own
    /// processes, so its head is the frontmost application whenever one is, and
    /// the previous one whenever this window is.
    #[must_use]
    fn profile_app(&self) -> Option<&ForegroundApp> {
        self.foreground.recent.first()
    }

    /// The name of the per-app profile the active device runs under, or `None`
    /// when it falls back to the device's global bindings — which is also what
    /// a device with no saved config, or a host with no readable foreground
    /// app, reports.
    #[must_use]
    pub fn active_profile_name(&self) -> Option<&str> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        let app = self.profile_app()?;
        self.config
            .has_app_override(key, &app.id)
            .then_some(app.display_name.as_str())
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

impl EventEmitter<StateEvent> for AppState {}
