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

use gpui::{App, Global};
use openlogi_core::config::{AppSettings, Appearance, Config, DeviceIdentity, Lighting};
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiCapabilities, DpiInfo, SmartShiftMode, SmartShiftStatus, WriteError,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

mod devices;
mod load;

pub use devices::DeviceRecord;
pub use load::{DpiStatus, Load, SmartShiftLoad};

use load::LazyDeviceData;

use crate::asset::AssetResolver;
use crate::data::mouse_buttons::{Action, Binding, ButtonId, GestureDirection};
use crate::state::devices::{build_device_list, pick_initial_device, sort_device_list};
use openlogi_agent_core::bindings::{bindings_for, gesture_bindings_for};

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
    smartshift_pending_confirm: std::collections::BTreeSet<String>,
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
        cache: &AssetResolver,
        ipc_commands: mpsc::UnboundedSender<crate::ipc_client::Command>,
    ) -> Self {
        let device_list = build_device_list(inventories, cache, &config);
        // Record any device probed at launch so it survives the next cold start.
        persist_identities(&mut config, &device_list);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let mut state = Self {
            current_device,
            current_app_bundle: None,
            active_button: None,
            // Updated from the agent's IPC poll; the GUI no longer runs the
            // hook, so it can't meaningfully query Accessibility (or devices)
            // itself.
            agent_link: AgentLink::Connecting,
            button_bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            dpi: DEFAULT_DPI,
            dpi_data: LazyDeviceData::default(),
            inventory_misses: BTreeMap::new(),
            smartshift_data: LazyDeviceData::default(),
            smartshift_pending_confirm: std::collections::BTreeSet::new(),
            device_list,
            config,
            ipc_commands,
            last_inventory: Vec::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            monitor_events: std::collections::VecDeque::new(),
            #[cfg(all(target_os = "macos", debug_assertions))]
            event_taps: Vec::new(),
        };
        state.button_bindings = state.bindings_for_current();
        state.gesture_bindings = state.gesture_bindings_for_current();
        state
    }

    /// Send a device command to the agent over IPC, logging a dropped channel
    /// (the client thread is gone) rather than surfacing it.
    fn send_ipc(&self, command: crate::ipc_client::Command) {
        if self.ipc_commands.send(command).is_err() {
            warn!("IPC client thread is gone — device command dropped");
        }
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, what, "could not persist to config.toml — agent reload skipped");
            return;
        }
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
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

    /// Append a batch of live-monitor events, capping the retained history so the
    /// buffer can't grow without bound while the monitor is open.
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn push_monitor_events(&mut self, events: Vec<openlogi_agent_core::ipc::MonitorEvent>) {
        const MAX: usize = 200;
        self.monitor_events.extend(events);
        let overflow = self.monitor_events.len().saturating_sub(MAX);
        self.monitor_events.drain(..overflow);
    }

    /// Recent live-monitor events, oldest first.
    #[cfg(all(target_os = "macos", debug_assertions))]
    #[must_use]
    pub fn monitor_events(
        &self,
    ) -> &std::collections::VecDeque<openlogi_agent_core::ipc::MonitorEvent> {
        &self.monitor_events
    }

    /// Replace the cached event-tap snapshot the Diagnostics page renders.
    /// Refreshed on the live-monitor poll tick; see [`Self::event_taps`].
    #[cfg(all(target_os = "macos", debug_assertions))]
    pub fn set_event_taps(&mut self, taps: Vec<openlogi_hook::EventTapInfo>) {
        self.event_taps = taps;
    }

    /// The cached event-tap snapshot for the Diagnostics page.
    #[cfg(all(target_os = "macos", debug_assertions))]
    #[must_use]
    pub fn event_taps(&self) -> &[openlogi_hook::EventTapInfo] {
        &self.event_taps
    }

    /// Config schema version and the number of devices with saved configuration.
    #[must_use]
    pub fn config_summary(&self) -> (u32, usize) {
        (self.config.schema_version, self.config.devices.len())
    }

    /// The cached DPI-discovery status for `key`, for the diagnostics report.
    #[must_use]
    pub fn dpi_status_for(&self, key: &str) -> Option<DpiStatus> {
        self.dpi_data.get(key).cloned()
    }

    /// Ask the agent to fire the macOS Accessibility prompt. The agent owns the
    /// CGEventTap, so the system dialog must name and authorize the *agent*
    /// binary; prompting in the GUI process (as the pre-split build did) would
    /// grant the wrong binary and the hook would never install.
    pub fn request_accessibility_prompt(&self) {
        self.send_ipc(crate::ipc_client::Command::RequestAccessibilityPrompt);
    }

    /// The active device, or `None` when [`Self::device_list`] is empty or
    /// `current_device` is past the end.
    #[must_use]
    pub fn current_record(&self) -> Option<&DeviceRecord> {
        self.device_list.get(self.current_device)
    }

    /// The agent connection state the render path branches on.
    #[must_use]
    pub fn agent_link(&self) -> &AgentLink {
        &self.agent_link
    }

    /// The latest agent status snapshot — `None` while not connected (any
    /// non-[`AgentLink::Ready`] state), which readers like the Settings
    /// permission rows surface as "unknown", not "denied".
    #[must_use]
    pub fn agent_status(&self) -> Option<&openlogi_agent_core::ipc::AgentStatus> {
        match &self.agent_link {
            AgentLink::Ready(status) => Some(status),
            _ => None,
        }
    }

    /// Replace the link, reporting whether it actually changed — the steady
    /// IPC poll mostly delivers identical snapshots, and the caller skips the
    /// window refresh for those.
    pub fn set_agent_link(&mut self, link: AgentLink) -> bool {
        if self.agent_link == link {
            return false;
        }
        self.agent_link = link;
        true
    }

    /// Replace [`Self::device_list`] from a fresh inventory snapshot,
    /// preserving the carousel selection by `config_key` when possible. If
    /// the previously-selected device disappeared, the selection falls back
    /// to index 0. Returns whether anything actually changed.
    ///
    /// No-op (returning `false`) when the new list has the same `config_key`
    /// sequence as the current one — the caller skips the window refresh, and
    /// quiet polling cycles cause no spurious re-renders (P1.6). `force`
    /// pushes through that early-return: the records embed resolved asset
    /// paths, so a completed asset sync needs one rebuild even though the
    /// device *set* is unchanged.
    pub fn refresh_inventories(
        &mut self,
        inventories: &[DeviceInventory],
        cache: &AssetResolver,
        force: bool,
    ) -> bool {
        let new_list = build_device_list(inventories, cache, &self.config);
        let merged_list = self.merge_inventory_snapshot(new_list);
        // Capture any newly-probed identity before the unchanged-check can early
        // out: a device whose capabilities just resolved keeps the same
        // config_key + route, so that guard would otherwise skip the write.
        persist_identities(&mut self.config, &merged_list);
        // Compare more than config_key: a device can reconnect on a new HID++
        // index while keeping its physical config key, and the fresh route must
        // replace the stale one so reads/writes don't target a dead index.
        // `online` and `capabilities` are compared too, so a device waking up or
        // a probe that resolves its feature table on a stable route still
        // refreshes the carousel (and its config panels) instead of being
        // swallowed by this guard.
        let unchanged = merged_list.len() == self.device_list.len()
            && merged_list
                .iter()
                .zip(self.device_list.iter())
                .all(|(a, b)| {
                    a.config_key == b.config_key
                        && a.route == b.route
                        && a.online == b.online
                        && a.capabilities == b.capabilities
                });
        if unchanged && !force {
            return false;
        }

        let previous_key = self.current_record().map(|r| r.config_key.clone());
        let new_index = previous_key
            .as_deref()
            .and_then(|k| merged_list.iter().position(|r| r.config_key == k))
            .unwrap_or(0);
        let connected_keys = merged_list
            .iter()
            .map(|r| r.config_key.as_str())
            .collect::<Vec<_>>();
        debug!(
            count = merged_list.len(),
            ?connected_keys,
            "inventory refreshed"
        );

        // A device that came back on a different route must re-discover DPI —
        // its cached status/attempts were keyed to the now-dead route.
        let rerouted: Vec<String> = merged_list
            .iter()
            .filter(|new| {
                self.device_list
                    .iter()
                    .any(|old| old.config_key == new.config_key && old.route != new.route)
            })
            .map(|new| new.config_key.clone())
            .collect();

        self.device_list = merged_list;
        for key in &rerouted {
            self.dpi_data.remove(key);
            self.smartshift_data.remove(key);
        }
        let present = |key: &str| {
            self.device_list
                .iter()
                .any(|r| r.config_key.as_str() == key)
        };
        self.dpi_data.retain_present(present);
        self.smartshift_data.retain_present(present);
        self.current_device = new_index;
        // The active device may have changed (selection fell back to index 0
        // when the previous one vanished); re-seed the displayed DPI so it
        // tracks the now-current device rather than the old one.
        self.dpi = self.dpi_for_current();
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        // Display state only — the agent runs its own inventory watcher and
        // rebuilds the live binding/DPI maps itself.
        true
    }

    fn merge_inventory_snapshot(&mut self, new_list: Vec<DeviceRecord>) -> Vec<DeviceRecord> {
        let mut by_key = new_list
            .into_iter()
            .map(|record| (record.config_key.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let mut merged = Vec::with_capacity(by_key.len().max(self.device_list.len()));

        for previous in &self.device_list {
            if let Some(record) = by_key.remove(&previous.config_key) {
                self.inventory_misses.remove(&previous.config_key);
                merged.push(record);
                continue;
            }

            let misses = self
                .inventory_misses
                .entry(previous.config_key.clone())
                .or_insert(0);
            *misses = misses.saturating_add(1);
            if *misses <= INVENTORY_MISS_GRACE {
                debug!(
                    key = %previous.config_key,
                    misses = *misses,
                    "keeping device through transient inventory miss"
                );
                merged.push(previous.clone());
            }
        }

        for (key, record) in by_key {
            self.inventory_misses.remove(&key);
            merged.push(record);
        }
        self.inventory_misses
            .retain(|key, _| merged.iter().any(|record| record.config_key == *key));
        // `merged` is `previous-order + newly-appeared`, so re-apply the
        // canonical route order or a new device would be stuck at the end of
        // the carousel permanently.
        sort_device_list(&mut merged);
        merged
    }

    /// Switch the carousel to `idx`. Out-of-range indices are silently
    /// ignored so callers can pass them straight through from UI events.
    /// Persists the new selection (by config key, not index — index isn't
    /// stable across restarts), reloads bindings for the new device, and
    /// pushes the new map into the hook-shared `Arc`.
    pub fn set_current_device(&mut self, idx: usize) {
        if idx >= self.device_list.len() || idx == self.current_device {
            return;
        }
        self.current_device = idx;
        // A device left in `Failed` (transient read errors exhausted its retry
        // budget) gets one fresh attempt each time it is re-selected.
        if let Some(key) = self.current_record().map(|r| r.config_key.clone()) {
            if matches!(self.dpi_data.get(&key), Some(Load::Failed(_))) {
                self.dpi_data.retry(&key);
            }
            if matches!(self.smartshift_data.get(&key), Some(Load::Failed(_))) {
                self.smartshift_data.retry(&key);
            }
        }
        // `self.dpi` is the active device's value; adopt the newly-selected
        // device's known DPI so the panel doesn't keep showing the previous
        // device's number until a fresh read lands.
        self.dpi = self.dpi_for_current();
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        let key = self.current_record().map(|r| r.config_key.clone());
        self.config.set_selected_device(key);
        // The agent owns the hook + device I/O; have it switch devices too.
        self.persist_and_reload("selected device");
    }

    /// Replace the DPI preset list for the currently selected device. The
    /// new list is persisted to `config.toml` and pushed into the shared
    /// hook map so the next `CycleDpiPresets` press sees it. The cycle
    /// `index` is reset to 0 — the user just rebuilt the list, the old
    /// index is meaningless.
    ///
    /// No-op when no device is selected (binding panel won't expose the
    /// editor in that state).
    pub fn commit_dpi_presets(&mut self, presets: Vec<u32>) {
        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!("no active device key — DPI presets kept in memory only");
            return;
        };
        self.config.set_dpi_presets(&key, presets);
        self.persist_and_reload("DPI presets");
    }

    /// Read the DPI preset list for the active device, or an empty `Vec`
    /// when no device is selected. UI helper.
    #[must_use]
    pub fn dpi_presets(&self) -> Vec<u32> {
        self.current_record()
            .map(|r| self.config.dpi_presets(&r.config_key))
            .unwrap_or_default()
    }

    /// DPI capability status for the active device.
    #[must_use]
    pub fn current_dpi_status(&self) -> DpiStatus {
        self.current_record().map_or(DpiStatus::Unknown, |record| {
            self.dpi_data.status(&record.config_key)
        })
    }

    /// Whether the active device still needs a DPI read (no status recorded —
    /// i.e. `Unknown`). Cheaper than `current_dpi_status() == Unknown`: it
    /// avoids cloning the `DpiInfo`, which matters on the per-frame render path.
    #[must_use]
    pub fn current_dpi_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.dpi_data.unqueried(&record.config_key))
    }

    /// The active device's known DPI, falling back to [`DEFAULT_DPI`] until its
    /// capability read completes. Used to seed `self.dpi` on a device switch.
    #[must_use]
    fn dpi_for_current(&self) -> u32 {
        self.current_record()
            .and_then(|record| self.dpi_data.get(&record.config_key))
            .and_then(|status| match status {
                DpiStatus::Ready(info) => Some(u32::from(info.current)),
                _ => None,
            })
            .unwrap_or(DEFAULT_DPI)
    }

    /// Mark DPI capability discovery as in flight for `key`.
    pub fn mark_dpi_loading(&mut self, key: &str) {
        self.dpi_data.mark_loading(key);
    }

    /// Reset a stuck `Loading` for `key` back to `Unknown`. Called when the
    /// discovery worker vanished without delivering a result (e.g. it panicked),
    /// so the device isn't wedged on "Reading…" with no path to retry.
    pub fn clear_dpi_loading(&mut self, key: &str) {
        self.dpi_data.clear_loading(key);
    }

    /// Drop the active device's recorded DPI status so the next render
    /// re-runs discovery. Backs the "click to retry" affordance on a
    /// [`DpiStatus::Failed`] device, which is the only recovery path when the
    /// carousel has a single device (re-selecting it is a no-op).
    pub fn retry_active_dpi(&mut self) {
        if let Some(key) = self.current_record().map(|r| r.config_key.clone()) {
            self.dpi_data.retry(&key);
        }
    }

    /// Store a DPI capability discovery result if it still matches the known
    /// device route. This guards against async reads completing after the
    /// carousel or inventory changed.
    pub fn store_dpi_info(
        &mut self,
        key: String,
        route: &DeviceRoute,
        result: Result<DpiInfo, WriteError>,
    ) {
        let is_active = self.current_record().map(|r| r.config_key.as_str()) == Some(key.as_str());
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.config_key == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.config_key == key);
        // Only the active device owns the shared `self.dpi`; a result landing for
        // a background device after a carousel switch must not clobber the
        // visible value.
        if let Some(info) = self.dpi_data.store(
            key,
            result,
            dpi_error_is_permanent,
            matches_route,
            still_present,
            "DPI",
        ) && is_active
        {
            self.dpi = u32::from(info.current);
        }
    }

    /// DPI capabilities for the active device, if discovery succeeded.
    #[must_use]
    pub fn active_dpi_capabilities(&self) -> Option<&DpiCapabilities> {
        self.current_record()
            .and_then(|record| self.dpi_data.get(&record.config_key))
            .and_then(|status| match status {
                DpiStatus::Ready(info) => Some(&info.capabilities),
                DpiStatus::Unknown
                | DpiStatus::Loading
                | DpiStatus::Failed(_)
                | DpiStatus::Unsupported(_) => None,
            })
    }

    /// Snap `dpi` to the active device's supported list when known.
    #[must_use]
    pub fn normalize_active_dpi(&self, dpi: u32) -> u32 {
        self.active_dpi_capabilities()
            .map_or(dpi, |caps| caps.snap(dpi))
    }

    /// SmartShift configuration status for the active device.
    #[must_use]
    pub fn current_smartshift_status(&self) -> SmartShiftLoad {
        self.current_record()
            .map_or(SmartShiftLoad::Unknown, |record| {
                self.smartshift_data.status(&record.config_key)
            })
    }

    /// Whether the active device still needs a SmartShift read (no status
    /// recorded). Cheaper than comparing a cloned [`SmartShiftLoad`] on the
    /// per-frame render path.
    #[must_use]
    pub fn current_smartshift_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.smartshift_data.unqueried(&record.config_key))
    }

    /// The active device's resolved SmartShift config, if the read succeeded.
    /// Callers use it to preserve fields they don't mean to change (e.g.
    /// tunable torque) when writing back.
    #[must_use]
    pub fn current_smartshift_ready(&self) -> Option<SmartShiftStatus> {
        self.current_record()
            .and_then(|record| self.smartshift_data.get(&record.config_key))
            .and_then(|status| match status {
                SmartShiftLoad::Ready(s) => Some(*s),
                SmartShiftLoad::Unknown
                | SmartShiftLoad::Loading
                | SmartShiftLoad::Failed(_)
                | SmartShiftLoad::Unsupported(_) => None,
            })
    }

    /// Mark SmartShift discovery as in flight for `key`.
    pub fn mark_smartshift_loading(&mut self, key: &str) {
        self.smartshift_data.mark_loading(key);
    }

    /// Reset a stuck `Loading` for `key` back to `Unknown` — called when the
    /// read worker vanished without delivering a result.
    pub fn clear_smartshift_loading(&mut self, key: &str) {
        self.smartshift_data.clear_loading(key);
    }

    /// Drop the active device's recorded SmartShift status so the next render
    /// re-runs discovery. Backs the "click to retry" affordance on a
    /// [`SmartShiftLoad::Failed`] device.
    pub fn retry_active_smartshift(&mut self) {
        if let Some(key) = self.current_record().map(|r| r.config_key.clone()) {
            self.smartshift_data.retry(&key);
        }
    }

    /// Store a SmartShift read result if it still matches the known device
    /// route, with the same transient-retry / permanent-unsupported handling
    /// as [`Self::store_dpi_info`].
    pub fn store_smartshift_status(
        &mut self,
        key: String,
        route: &DeviceRoute,
        result: Result<SmartShiftStatus, WriteError>,
    ) {
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.config_key == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.config_key == key);
        self.smartshift_data.store(
            key,
            result,
            smartshift_error_is_permanent,
            matches_route,
            still_present,
            "SmartShift",
        );
    }

    /// Write a full SmartShift configuration to the active device (best-effort,
    /// on a background thread), optimistically cache it, and persist it to
    /// `config.toml` — the values live in device RAM and reset on a power
    /// cycle (#189), so the agent re-applies them when the device reconnects.
    /// No-op when no device is selected.
    pub fn commit_smartshift(
        &mut self,
        mode: SmartShiftMode,
        auto_disengage: u8,
        tunable_torque: u8,
    ) {
        let Some(record) = self.current_record() else {
            debug!("no active device — SmartShift change ignored");
            return;
        };
        let key = record.config_key.clone();
        let route = record.route.clone();
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetSmartShift(
                route,
                mode,
                auto_disengage,
                tunable_torque,
            ));
        }
        self.config.set_smartshift(
            &key,
            openlogi_core::config::SmartShift {
                mode: mode.into(),
                auto_disengage,
                tunable_torque,
            },
        );
        self.persist_and_reload("SmartShift");
        // Reflect the write immediately so the panel doesn't flicker back to
        // the previous value before a re-read lands, but queue a confirming
        // re-read: the write is fire-and-forget, so a sleeping device that
        // rejected or timed it out would otherwise leave this optimistic value
        // showing as "applied" forever (Ready blocks any further read).
        self.smartshift_data.set_ready(
            key.clone(),
            SmartShiftStatus {
                mode,
                auto_disengage,
                tunable_torque,
            },
        );
        self.smartshift_pending_confirm.insert(key);
    }

    /// Whether the active device's scroll wheel is inverted (issue #126).
    /// `false` when no device is selected or the device hasn't opted in.
    #[must_use]
    pub fn current_invert_scroll(&self) -> bool {
        self.current_record()
            .is_some_and(|r| self.config.invert_scroll(&r.config_key))
    }

    /// Whether the active device reports native HID++ wheel inversion support.
    #[must_use]
    pub fn current_scroll_inversion_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.scroll_inversion)
    }

    /// Set the active device's scroll-wheel inversion, persist it, and reload
    /// the agent so it writes the device's native HID++ wheel inversion. No-op
    /// when no device is selected or the active device does not report support.
    pub fn commit_invert_scroll(&mut self, invert: bool) {
        if !self.current_scroll_inversion_supported() {
            debug!("active device does not support native scroll inversion");
            return;
        }
        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!("no active device — invert-scroll change ignored");
            return;
        };
        self.config.set_invert_scroll(&key, invert);
        self.persist_and_reload("invert scroll");
    }

    /// Take the active device's pending SmartShift confirm, if any. Returns the
    /// `(config_key, route)` for a one-shot re-read that replaces the optimistic
    /// value with the device's real state; consumed once so it doesn't re-fire.
    pub fn take_active_smartshift_confirm(&mut self) -> Option<(String, DeviceRoute)> {
        let record = self.current_record()?;
        let key = record.config_key.clone();
        let route = record.route.clone()?;
        self.smartshift_pending_confirm
            .remove(&key)
            .then_some((key, route))
    }

    /// The lighting config for the active device, or the default when none is
    /// stored / no device is selected.
    #[must_use]
    pub fn lighting(&self) -> Lighting {
        self.current_record()
            .and_then(|r| self.config.lighting(&r.config_key))
            .unwrap_or_default()
    }

    /// The stored lighting config for `key`, or `None` when unset.
    #[must_use]
    pub fn lighting_for(&self, key: &str) -> Option<Lighting> {
        self.config.lighting(key)
    }

    /// Persist a new lighting config for the active device and push it to the
    /// hardware (best-effort). No-op when no device is selected.
    pub fn commit_lighting(&mut self, lighting: Lighting) {
        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!("no active device key — lighting kept in memory only");
            return;
        };
        let target = self.current_record().and_then(|r| r.route.clone());
        if let Some(route) = target {
            self.send_ipc(crate::ipc_client::Command::SetLighting(
                route,
                lighting.clone(),
            ));
        }
        self.config.set_lighting(&key, lighting);
        // Keep the agent's config copy fresh: it re-applies the saved colour
        // when the keyboard reconnects, and without the reload it would
        // replay whatever was saved the last time something *else* reloaded.
        self.persist_and_reload("lighting");
    }

    /// Apply `dpi` to the active device (best-effort, via the agent) and
    /// persist it per device — the sensor value lives in device RAM and resets
    /// on a power cycle (#189), so the agent re-applies it on reconnect.
    /// Updates the displayed value even with no device selected.
    pub fn commit_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        let Some(record) = self.current_record() else {
            debug!("no active device — DPI change kept in memory only");
            return;
        };
        let key = record.config_key.clone();
        let route = record.route.clone();
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetDpi(route, dpi));
        }
        self.config.set_dpi(&key, dpi);
        self.persist_and_reload("DPI");
    }

    /// App-wide settings backing the Settings window (launch-at-login,
    /// update check). Read-only view; mutate via the setters below so the
    /// change is persisted.
    #[must_use]
    pub fn app_settings(&self) -> &AppSettings {
        &self.config.app_settings
    }

    /// Toggle launch-at-login, persist to `config.toml`, and reconcile the
    /// macOS `LaunchAgent` plist so the change takes effect without a
    /// restart. No-op when the value is unchanged. Disk failures are logged,
    /// not propagated — the Settings UI shouldn't crash on a full volume.
    pub fn set_launch_at_login(&mut self, enabled: bool) {
        if self.config.app_settings.launch_at_login == enabled {
            return;
        }
        self.config.app_settings.launch_at_login = enabled;
        // The agent owns autostart now; it reconciles its LaunchAgent (which
        // points at the agent, not the GUI) when it reloads the config.
        self.persist_and_reload("launch-at-login setting");
    }

    /// Toggle the menu-bar (status item) icon preference and persist it. The
    /// icon is hosted by the always-on agent, which reads this on startup and
    /// installs the status item only when enabled — so the change takes effect
    /// the next time the agent launches (a no-restart live toggle would need a
    /// main-thread hop from the agent's IPC reload). `ReloadConfig` keeps the
    /// agent's other config in sync meanwhile. No-op when unchanged.
    ///
    /// The only caller is the macOS-gated menu-bar toggle in Settings, so the
    /// setter is gated the same way to stay dead-code-clean on other targets.
    #[cfg(target_os = "macos")]
    pub fn set_show_in_menu_bar(&mut self, enabled: bool) {
        if self.config.app_settings.show_in_menu_bar == enabled {
            return;
        }
        self.config.app_settings.show_in_menu_bar = enabled;
        self.persist_and_reload("show-in-menu-bar setting");
    }

    /// Toggle the opt-in update check and persist it. No immediate side
    /// effect beyond the next launch reading the new value. No-op when
    /// unchanged.
    pub fn set_check_for_updates(&mut self, enabled: bool) {
        if self.config.app_settings.check_for_updates == enabled {
            return;
        }
        self.config.app_settings.check_for_updates = enabled;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist update-check setting");
        }
    }

    /// Toggle opt-in automatic install and persist it. The launch-time updater
    /// observer reads this live, so a newer version found after this is enabled
    /// downloads and stages on its own; no immediate side effect here. No-op
    /// when unchanged.
    pub fn set_auto_install_updates(&mut self, enabled: bool) {
        if self.config.app_settings.auto_install_updates == enabled {
            return;
        }
        self.config.app_settings.auto_install_updates = enabled;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist auto-install setting");
        }
    }

    /// Persist the light/dark appearance preference. The caller re-applies the
    /// live theme via [`crate::theme::apply_from_settings`]; this only writes the
    /// choice. No-op when unchanged.
    pub fn set_appearance(&mut self, appearance: Appearance) {
        if self.config.app_settings.appearance == appearance {
            return;
        }
        self.config.app_settings.appearance = appearance;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist appearance setting");
        }
    }

    /// Persist the chosen theme name for one mode (`None` = the OpenLogi brand
    /// theme). No-op when unchanged.
    pub fn set_theme(&mut self, dark: bool, name: Option<String>) {
        let slot = if dark {
            &mut self.config.app_settings.theme_dark
        } else {
            &mut self.config.app_settings.theme_light
        };
        if *slot == name {
            return;
        }
        *slot = name;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist theme setting");
        }
    }

    /// Persist the UI corner-radius override (`None` = each theme's own radius).
    /// No-op when unchanged.
    pub fn set_ui_radius(&mut self, radius: Option<u8>) {
        if self.config.app_settings.ui_radius == radius {
            return;
        }
        self.config.app_settings.ui_radius = radius;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist UI radius setting");
        }
    }

    /// Set the thumb-wheel sensitivity (clamped to the valid range), publish it
    /// to the gesture watcher via the shared atomic, and persist it. No-op when
    /// unchanged. Disk failures are logged, not propagated.
    pub fn set_thumbwheel_sensitivity(&mut self, sensitivity: i32) {
        let sensitivity = sensitivity.clamp(
            openlogi_core::config::MIN_THUMBWHEEL_SENSITIVITY,
            openlogi_core::config::MAX_THUMBWHEEL_SENSITIVITY,
        );
        if self.config.app_settings.thumbwheel_sensitivity == sensitivity {
            return;
        }
        self.config.app_settings.thumbwheel_sensitivity = sensitivity;
        self.persist_and_reload("thumbwheel sensitivity");
    }

    pub fn set_auto_download_assets(&mut self, enabled: bool) {
        if self.config.app_settings.auto_download_assets == enabled {
            return;
        }
        self.config.app_settings.auto_download_assets = enabled;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist auto-download-assets setting");
        }
    }

    /// Record the answer to the first-run update-check prompt: enable (or leave
    /// disabled) the check, and mark the prompt as seen so it never reappears.
    /// Persists once.
    pub fn record_update_consent(&mut self, enabled: bool) {
        self.config.app_settings.check_for_updates = enabled;
        self.config.app_settings.update_prompt_seen = true;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist update-check consent");
        }
    }

    /// The stored UI-language preference: `Some(code)` for an explicit choice,
    /// `None` for "follow system". Distinct from the *active* locale that
    /// `None` resolves to at startup, so the Settings picker can show "Follow
    /// system" as the selected option.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.config.app_settings.language.as_deref()
    }

    /// Set the UI language (`None` = follow system), persist it, switch the
    /// process-global locale live via [`crate::i18n`], and repaint open UI.
    /// No-op when unchanged.
    pub fn set_language(&mut self, language: Option<String>, cx: &mut App) {
        if self.config.app_settings.language == language {
            return;
        }
        self.config.app_settings.language = language;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist language setting");
        }
        crate::i18n::activate(self.config.app_settings.language.as_deref());
        cx.refresh_windows();
        crate::app_menu::rebuild(cx);
    }

    /// Update a single binding in memory, on disk, and in the shared hook
    /// map for the currently selected device.
    ///
    /// Disk failures and poisoned hook locks are logged at `warn` instead
    /// of bubbling up: the UI thread shouldn't crash because the user's
    /// home volume is full or because the hook thread panicked.
    pub fn commit_binding(&mut self, button: ButtonId, action: Action) {
        self.button_bindings.insert(button, action.clone());

        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!(
                ?button,
                "no active device key — binding kept in memory only"
            );
            return;
        };
        self.config
            .set_binding(&key, button, Binding::Single(action));
        // The agent owns the hook; have it rebuild its live map from config.
        self.persist_and_reload("binding");
    }

    fn bindings_for_current(&self) -> BTreeMap<ButtonId, Action> {
        bindings_for(
            &self.config,
            self.current_record().map(|r| r.config_key.as_str()),
            self.current_app_bundle.as_deref(),
        )
    }

    fn gesture_bindings_for_current(&self) -> BTreeMap<GestureDirection, Action> {
        let Some(key) = self.current_record().map(|r| r.config_key.as_str()) else {
            return BTreeMap::new();
        };
        match self.config.gesture_owner(key) {
            // The HID++ gesture button seeds every direction from the defaults.
            Some(ButtonId::GestureButton) => gesture_bindings_for(&self.config, Some(key)),
            // A promoted OS-hook button is shown from its raw stored map (which
            // `set_gesture_owner` seeds with full defaults), so the menu matches
            // exactly what `oshook_gestures_for` dispatches — no seeding here.
            Some(owner) => match self.config.bindings_for(key).get(&owner) {
                Some(Binding::Gesture(map)) => map.clone(),
                _ => BTreeMap::new(),
            },
            None => BTreeMap::new(),
        }
    }

    /// The current device's gesture button — the [`Binding::Gesture`] owner — or
    /// `None` when no button is in gesture mode. Drives which button's card opens
    /// the gesture menu rather than the single-action picker.
    #[must_use]
    pub fn current_gesture_owner(&self) -> Option<ButtonId> {
        let key = self.current_record()?.config_key.as_str();
        self.config.gesture_owner(key)
    }

    /// Make `button` the current device's gesture button (or clear it with
    /// `None`), enforcing the one-gesture-button-per-device lock. Persists, tells
    /// the agent to rebuild, and refreshes the projected maps the UI reads.
    pub fn commit_gesture_owner(&mut self, button: Option<ButtonId>) {
        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            return;
        };
        match button {
            Some(b) => {
                self.config.set_gesture_owner(&key, b);
            }
            None => {
                self.config.disable_gestures(&key);
            }
        }
        // The owner change shuffles bindings between the single + gesture maps.
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        self.persist_and_reload("gesture-button change");
    }

    /// Update a single gesture-button sub-binding in memory, on disk, and in the
    /// shared gesture map the watcher thread reads.
    pub fn commit_gesture_binding(&mut self, direction: GestureDirection, action: Action) {
        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!(
                ?direction,
                "no active device key — gesture binding edit ignored"
            );
            return;
        };
        // Edit whichever button owns gestures — not always the HID++ gesture button. When
        // gestures are off, a stray edit must NOT silently re-enable them on the
        // default owner (the gesture editor shouldn't be reachable in that state):
        // no-op instead.
        let Some(owner) = self.config.gesture_owner(&key) else {
            debug!(
                ?direction,
                "gestures are off — ignoring gesture binding edit"
            );
            return;
        };
        self.gesture_bindings.insert(direction, action.clone());
        self.config
            .set_gesture_direction(&key, owner, direction, action);
        // The agent owns the gesture watcher; have it rebuild from config.
        self.persist_and_reload("gesture binding");
    }
}

/// Record the identity (name / kind / capabilities) of every currently online,
/// fully-probed device into `config`, persisting to disk only when something
/// actually changed.
///
/// This is the write half of the identity-driven device list: it is what lets
/// [`build_device_list`] resurrect a sleeping device on the next launch. Only
/// online devices with *measured* capabilities are recorded — never a presumed
/// or carried-forward `None` — so a placeholder never persists empty panels.
/// The change-guard keeps quiet inventory ticks off the disk; the agent does
/// not consume identities, so no `ReloadConfig` is sent.
fn persist_identities(config: &mut Config, list: &[DeviceRecord]) {
    let mut changed = false;
    for record in list {
        if !record.online {
            continue;
        }
        let Some(capabilities) = record.capabilities else {
            continue;
        };
        let identity = DeviceIdentity {
            display_name: record.display_name.clone(),
            kind: record.kind,
            capabilities,
            model_info: record.model_info.clone().map(|mut model| {
                model.serial_number = None;
                model.unit_id = [0; 4];
                model
            }),
            codename: record.codename.clone(),
        };
        if config.device_identity(&record.config_key) != Some(&identity) {
            config.set_device_identity(&record.config_key, identity);
            changed = true;
        }
    }
    if changed && let Err(e) = config.save_atomic() {
        warn!(error = %e, "could not persist device identities to config.toml");
    }
}

/// Whether a DPI discovery error is permanent (the device genuinely lacks the
/// feature or reports nothing usable) versus transient (a timeout or busy
/// device worth retrying).
fn dpi_error_is_permanent(error: &WriteError) -> bool {
    matches!(
        error,
        WriteError::FeatureUnsupported { .. } | WriteError::EmptyDpiList
    )
}

/// Whether a SmartShift read error is permanent: a genuine "feature not
/// supported" reply (the device lacks `0x2111`) never changes, so stop
/// probing. Everything else (timeouts, busy device) is transient.
fn smartshift_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

impl Global for AppState {}
