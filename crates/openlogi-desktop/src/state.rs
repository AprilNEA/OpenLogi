//! App-wide UI state owned by a GPUI entity.
//!
//! Anything that more than one view needs to read (current device, currently
//! armed button, the DPI value the panel and the dot-preview share) lives
//! here. Per-component scratch state (hover index) stays
//! in the owning entity.
//!
//! [`AppState::with_runtime`] resolves every paired device's asset + DPI
//! target up front so views can switch instantly when the active device
//! changes — no synchronous I/O during the device switch.

use std::collections::{BTreeMap, BTreeSet};

use gpui::{App, Context, Entity, EventEmitter, Global};
use openlogi_core::app::ForegroundApp;
use openlogi_core::config::Config;
use openlogi_core::device::{DeviceInventory, StandaloneDevice};
use openlogi_core::hid::{Dpi, SmartShiftStatus};
use tokio::sync::mpsc;
use tracing::warn;

pub use config::ConfigPersistence;
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

use agent::AgentSession;
use bindings::BindingState;
use device_store::DeviceStore;
pub(crate) use devices::camera_model_info;
use light::LightingState;
use pointer::PointerState;

use crate::services::assets::AssetResolver;
use crate::services::device_reads::DeviceReads;
use crate::state::config::ConfigState;
use crate::state::devices::{build_device_list, pick_initial_device};

mod action_ring;
mod agent;
mod bindings;
mod camera;
mod config;
mod device_key;
mod device_runtime;
mod device_store;
mod devices;
mod dpi;
mod inventory;
mod light;
mod lighting;
mod load;
mod pointer;
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
    /// The active device changed.
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
    /// The interface language switched live. Views re-render localized strings
    /// on the accompanying refresh; this event is for localized text *cached
    /// in state*, which must be recomputed in the new locale.
    LanguageChanged,
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

/// Inventory snapshots can briefly miss a real device while another HID++
/// request is in flight. Keep the previous record through this many
/// consecutive misses so a transient probe timeout does not make the device card
/// disappear mid-interaction.
const INVENTORY_MISS_GRACE: u8 = 2;

pub struct AppState {
    /// Live configuration and its last persisted rollback point.
    config: ConfigState,
    /// Agent-owned observations accepted by this GUI session.
    agent: AgentSession,
    /// Merged device catalog, valid active selection, and per-device runtime.
    devices: DeviceStore,
    /// Binding-editor scope and projections derived from config.
    bindings: BindingState,
    /// Per-device Actions Ring profile open in this window's editor.
    action_ring_editing_apps: BTreeMap<String, String>,
    /// DPI/SmartShift reads and the active pointer editor value.
    pointer: PointerState,
    /// Standalone-light sequencing and aggregate camera activity.
    lighting: LightingState,
    /// Sender to the IPC client thread. The agent owns the hook and device I/O.
    ipc_commands: mpsc::UnboundedSender<crate::services::ipc::Command>,
    /// Devices whose onboard firmware map was already fetched this session.
    onboard_import_attempted: BTreeSet<DeviceKey>,
    /// Camera-consent poll started by an in-app macOS prompt. The app-state
    /// entity owns it because permission can resolve after the initiating view
    /// or window closes; dropping the entity at process shutdown cancels it.
    #[cfg(target_os = "macos")]
    camera_permission_poll: Option<gpui::Task<()>>,
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
            state.load_onboard_bindings(cx);
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
        config: Config,
        inventories: &[DeviceInventory],
        standalone: &[StandaloneDevice],
        cache: &AssetResolver,
        cameras: &[openlogi_camera::Camera],
        config_persistence: ConfigPersistence,
        ipc_commands: mpsc::UnboundedSender<crate::services::ipc::Command>,
    ) -> Self {
        let mut config = ConfigState::new(config, config_persistence);
        let device_list = build_device_list(inventories, standalone, cache, &config, cameras);
        // Fold each online device's route into its canonical entry before
        // anything writes to the config — the first frame after a schema-5
        // upgrade is the earliest moment this can happen, and the ordering
        // against `persist_identities` matters for the same reason it does in
        // `refresh_inventories`: identities keyed off a legacy `config_key`
        // would leave a bare canonical entry beside the un-folded legacy one.
        let adopted = config.edit(|config| inventory::adopt_routes(config, &device_list));
        // Adoption re-keyed entries, so rebuild before reading the list again.
        let device_list = if adopted {
            build_device_list(inventories, standalone, cache, &config, cameras)
        } else {
            device_list
        };
        // Record any device probed at launch so it survives the next cold start.
        let identities_changed =
            config.edit(|config| inventory::persist_identities(config, &device_list));
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let bindings = BindingState::new(
            &config,
            device_list
                .get(current_device)
                .and_then(DeviceRecord::persistent_config_key),
        );
        let mut state = Self {
            config,
            agent: AgentSession::default(),
            devices: DeviceStore::new(device_list, current_device),
            bindings,
            action_ring_editing_apps: BTreeMap::new(),
            pointer: PointerState::default(),
            lighting: LightingState::default(),
            ipc_commands,
            onboard_import_attempted: BTreeSet::new(),
            #[cfg(target_os = "macos")]
            camera_permission_poll: None,
        };
        // Plain `persist_config`, not `persist_and_reload` as
        // `refresh_inventories` uses for the same fold: there is no agent
        // connection to reload yet this early in startup, and the
        // unconditional `ReloadConfig` send at the end of this constructor
        // (below) already covers both branches once `state` is fully built —
        // sending it here too would just queue a second, redundant reload.
        if adopted {
            state.persist_config("adopt device route");
        } else if identities_changed {
            state.persist_config("device identity");
        }
        if state.config.should_reload_agent() {
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
        if self.config.persist(what) {
            true
        } else {
            self.restore_config_projections();
            false
        }
    }

    fn restore_config_projections(&mut self) {
        self.restore_binding_projections();
        if let Some(dpi) = self.current_record().and_then(|record| {
            record
                .persistent_config_key()
                .and_then(|key| self.config.devices.get(key))
                .and_then(|device| device.effective_dpi(&record.route_key))
        }) {
            self.pointer.dpi = dpi;
        }
    }

    /// Current config failure, shown as a fail-closed whole-window notice.
    #[must_use]
    pub fn config_issue(&self) -> Option<&str> {
        self.config.issue()
    }

    /// Record whether the agent adopted the last saved config.
    pub fn apply_config_reload_result(
        &mut self,
        result: Result<(), openlogi_ipc::ConfigReloadError>,
    ) -> bool {
        self.config.apply_reload_result(result)
    }
    /// A clone of the IPC command sender used by the state entity to issue
    /// device reads and writes through the agent.
    #[must_use]
    pub fn ipc_sender(&self) -> mpsc::UnboundedSender<crate::services::ipc::Command> {
        self.ipc_commands.clone()
    }

    pub(crate) fn device_reads_mut(&mut self) -> &mut DeviceReads {
        &mut self.pointer.reads
    }
    /// Config schema version and the number of devices with saved configuration.
    #[must_use]
    pub fn config_summary(&self) -> (u32, usize) {
        (self.config.schema_version, self.config.devices.len())
    }
    /// All devices in deterministic gallery order.
    #[must_use]
    pub fn devices(&self) -> &[DeviceRecord] {
        &self.devices.records
    }

    /// The selected gallery index, or `None` when there are no devices.
    #[must_use]
    pub fn selected_device_index(&self) -> Option<usize> {
        self.devices.selected_index()
    }

    /// The active device, or `None` when the catalog is empty.
    #[must_use]
    pub fn current_record(&self) -> Option<&DeviceRecord> {
        self.devices.current()
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
    /// see [`openlogi_ipc::ForegroundApps`].
    pub fn recent_apps(&self) -> impl Iterator<Item = (&str, &str)> {
        self.foreground()
            .recent
            .iter()
            .map(|app| (app.id.as_str(), app.display_name.as_str()))
    }

    /// The name the agent last reported for `app`, or `None` for one it has not
    /// seen this session — a hand-written profile, or one carried in from
    /// another machine.
    #[must_use]
    pub fn recent_app_name(&self, app: &str) -> Option<&str> {
        self.foreground()
            .recent
            .iter()
            .find(|seen| seen.id == app)
            .map(|seen| seen.display_name.as_str())
    }

    /// The application whose profile the user is asking about.
    ///
    /// Not [`openlogi_ipc::ForegroundApps::current`]: while this window has
    /// focus *OpenLogi* is the frontmost application, so the app the user means
    /// is the one they came from. The recent list is exactly that — it excludes
    /// OpenLogi's own processes, so its head is the frontmost application
    /// whenever one is, and the previous one whenever this window is.
    #[must_use]
    fn profile_app(&self) -> Option<&ForegroundApp> {
        self.foreground().recent.first()
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
}

impl EventEmitter<StateEvent> for AppState {}
