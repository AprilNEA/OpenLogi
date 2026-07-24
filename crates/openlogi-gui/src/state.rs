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
use openlogi_core::config::{
    AppSettings, Appearance, AssetSourcePreference, Config, DeviceIdentity, Lighting,
};
use openlogi_core::device::{DeviceInventory, DeviceModelInfo};
use openlogi_hid::{
    DeviceRoute, DpiCapabilities, DpiInfo, OnboardProfilesInfo, ProfilesMode, SmartShiftMode,
    SmartShiftStatus, WriteError,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

mod devices;
mod load;

pub use devices::DeviceRecord;
pub use load::{DpiStatus, Load, ProfilesLoad, SmartShiftLoad};

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

use load::LazyDeviceData;

use crate::asset::AssetResolver;
use crate::data::mouse_buttons::{Action, Binding, ButtonId, GestureDirection};
use crate::state::devices::{
    adopt_transient_record, build_device_list, direct_key_prefix, pick_initial_device,
    sort_device_list,
};
use openlogi_agent_core::bindings::{bindings_for, gesture_bindings_for};
use openlogi_agent_core::device_order::PhysicalDeviceKey;

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
    smartshift_pending_confirm: BTreeMap<String, u64>,
    /// Monotonic identity assigned to the next confirmable SmartShift write.
    next_smartshift_write_id: u64,
    /// Visible outcome of the post-write SmartShift confirmation.
    smartshift_write_status: BTreeMap<String, SmartShiftWriteStatus>,
    /// Onboard-profiles (`0x8100`) state keyed by
    /// [`DeviceRecord::config_key`], loaded lazily on the same pattern as
    /// [`Self::smartshift_data`].
    profiles_data: LazyDeviceData<OnboardProfilesInfo>,
    /// Devices whose onboard-profiles config was just written optimistically
    /// and still need a confirming re-read, keyed by
    /// [`DeviceRecord::config_key`]. A plain set: the re-read only replaces the
    /// optimistic value with the device's actual state, and the panel shows no
    /// per-write outcome, so there is nothing to match a reply against.
    profiles_pending_confirm: std::collections::BTreeSet<String>,
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
            smartshift_pending_confirm: BTreeMap::new(),
            next_smartshift_write_id: 0,
            smartshift_write_status: BTreeMap::new(),
            profiles_data: LazyDeviceData::default(),
            profiles_pending_confirm: std::collections::BTreeSet::new(),
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

    /// Every known device model that can be resolved to an asset depot.
    ///
    /// This reads the UI's merged device list rather than only the latest live
    /// inventory, so a temporarily incomplete probe can still download art for
    /// a device restored from its persisted identity.
    pub(crate) fn asset_models(&self) -> Vec<(DeviceModelInfo, Option<String>)> {
        self.device_list
            .iter()
            .filter_map(|record| {
                record
                    .model_info
                    .clone()
                    .map(|model| (model, record.codename.clone()))
            })
            .collect()
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
            self.smartshift_pending_confirm.remove(key);
            self.smartshift_write_status.remove(key);
            self.profiles_data.remove(key);
        }
        let present = |key: &str| {
            self.device_list
                .iter()
                .any(|r| r.config_key.as_str() == key)
        };
        self.dpi_data.retain_present(present);
        self.smartshift_data.retain_present(present);
        self.smartshift_pending_confirm
            .retain(|key, _| present(key));
        self.smartshift_write_status.retain(|key, _| present(key));
        self.profiles_data.retain_present(present);
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
        let mut adopted = self.adopt_transient_records(&mut by_key);
        let mut merged = Vec::with_capacity(by_key.len().max(self.device_list.len()));

        for previous in &self.device_list {
            if let Some(record) = by_key.remove(&previous.config_key) {
                self.inventory_misses.remove(&previous.config_key);
                merged.push(record);
                continue;
            }

            if let Some(record) = adopted.remove(&previous.config_key) {
                self.inventory_misses.remove(&previous.config_key);
                merged.push(record);
                continue;
            }

            // An all-zero direct unit id is only a transient probe result. If
            // the next snapshot resolves a physical serial/unit key, retaining
            // this record through the normal miss grace would show both cards.
            if !previous.is_persistent() {
                self.inventory_misses.remove(&previous.config_key);
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
        // Adopted records whose known card was never in the previous list
        // (identity known only from config) still belong in the carousel.
        merged.extend(adopted.into_values());
        self.inventory_misses
            .retain(|key, _| merged.iter().any(|record| record.config_key == *key));
        // `merged` is `previous-order + newly-appeared`, so re-apply the
        // canonical route order or a new device would be stuck at the end of
        // the carousel permanently.
        sort_device_list(&mut merged);
        merged
    }

    /// Pair each transient direct record in the snapshot with the device it
    /// physically is. A transient key (`…:unit:00000000`) is a half-read probe
    /// of some existing device, not a new one (#482): when exactly one known
    /// card sharing its `direct:<vid>:<pid>` wire identity is not live online —
    /// so the half-read probe can only be that device — the transient record is
    /// folded into that card instead of surfacing beside it (or evicting it).
    /// With no such card the transient is dropped as probe noise when its wire
    /// product is already live online, and an ambiguous one (two known
    /// same-model cards absent) is left alone.
    fn adopt_transient_records(
        &self,
        by_key: &mut BTreeMap<String, DeviceRecord>,
    ) -> BTreeMap<String, DeviceRecord> {
        let transient_keys: Vec<String> = by_key
            .values()
            .filter(|record| !record.is_persistent())
            .map(|record| record.config_key.clone())
            .collect();
        let mut adopted = BTreeMap::new();
        for key in transient_keys {
            let Some(prefix) = direct_key_prefix(&key) else {
                continue;
            };
            let same_wire = |key: &str, record: &DeviceRecord| {
                record.is_persistent() && direct_key_prefix(key) == Some(prefix)
            };
            // A live online sibling is accounted for and never a candidate,
            // but it must not discard the transient — the half-read probe may
            // be the *other* same-model device.
            let mut candidates: Vec<String> = by_key
                .iter()
                .filter(|(k, record)| same_wire(k, record) && !record.online)
                .map(|(k, _)| k.clone())
                .collect();
            for previous in &self.device_list {
                if same_wire(&previous.config_key, previous)
                    && !by_key.contains_key(&previous.config_key)
                    && !candidates.contains(&previous.config_key)
                {
                    candidates.push(previous.config_key.clone());
                }
            }
            let [known_key] = candidates.as_slice() else {
                if candidates.is_empty()
                    && by_key
                        .iter()
                        .any(|(k, record)| same_wire(k, record) && record.online)
                {
                    by_key.remove(&key);
                }
                continue;
            };
            // Last tick's record carries the freshest identity; the offline
            // placeholder built from config is the fallback.
            let known = self
                .device_list
                .iter()
                .find(|record| record.config_key == *known_key)
                .cloned()
                .or_else(|| by_key.get(known_key).cloned());
            let Some(known) = known else {
                continue;
            };
            let known_key = known_key.clone();
            by_key.remove(&known_key);
            if let Some(live) = by_key.remove(&key) {
                adopted.insert(known_key, adopt_transient_record(&known, live));
            }
        }
        adopted
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
                self.smartshift_write_status.remove(&key);
            }
            if matches!(self.profiles_data.get(&key), Some(Load::Failed(_))) {
                self.profiles_data.retry(&key);
            }
        }
        // `self.dpi` is the active device's value; adopt the newly-selected
        // device's known DPI so the panel doesn't keep showing the previous
        // device's number until a fresh read lands.
        self.dpi = self.dpi_for_current();
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("transient device selection not persisted");
            return;
        };
        self.config.set_selected_device(Some(key));
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
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — DPI presets kept in memory only");
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
            .and_then(DeviceRecord::persistent_config_key)
            .map(|key| self.config.dpi_presets(key))
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

    /// Post-write confirmation status for the active device.
    #[must_use]
    pub fn current_smartshift_write_status(&self) -> Option<SmartShiftWriteStatus> {
        self.current_record().and_then(|record| {
            self.smartshift_write_status
                .get(&record.config_key)
                .copied()
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
            self.smartshift_write_status.remove(&key);
        }
    }

    /// Store a SmartShift read result if it still matches the known device
    /// route and write identity, with the same transient-retry /
    /// permanent-unsupported handling as [`Self::store_dpi_info`].
    pub fn store_smartshift_status(
        &mut self,
        key: String,
        route: &DeviceRoute,
        write_id: Option<u64>,
        result: Result<SmartShiftStatus, WriteError>,
    ) {
        if !smartshift_read_is_current(write_id, self.smartshift_write_status.get(&key)) {
            debug!(key, ?write_id, "stale SmartShift read result ignored");
            return;
        }
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.config_key == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.config_key == key);
        let status_key = key.clone();
        self.smartshift_data.store(
            key,
            result,
            smartshift_error_is_permanent,
            matches_route,
            still_present,
            "SmartShift",
        );
        let expected = match self.smartshift_write_status.get(&status_key) {
            Some(SmartShiftWriteStatus::Applying { expected, .. }) => Some(*expected),
            Some(SmartShiftWriteStatus::Confirmed | SmartShiftWriteStatus::Failed) | None => None,
        };
        if let Some(status) = expected.and_then(|expected| {
            smartshift_write_outcome(expected, self.smartshift_data.get(&status_key))
        }) {
            self.smartshift_write_status.insert(status_key, status);
        }
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
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        let can_confirm = route.is_some();
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetSmartShift(
                route,
                mode,
                auto_disengage,
                tunable_torque,
            ));
        }
        if let Some(persistent_key) = persistent_key {
            self.config.set_smartshift(
                &persistent_key,
                openlogi_core::config::SmartShift {
                    mode: mode.into(),
                    auto_disengage,
                    tunable_torque,
                },
            );
            self.persist_and_reload("SmartShift");
        }
        // Reflect the write immediately so the panel doesn't flicker back to
        // the previous value before a re-read lands, but queue a confirming
        // re-read: the write is fire-and-forget, so a sleeping device that
        // rejected or timed it out would otherwise leave this optimistic value
        // showing as "applied" forever (Ready blocks any further read).
        let expected = SmartShiftStatus {
            mode,
            auto_disengage,
            tunable_torque,
        };
        self.smartshift_data.set_ready(key.clone(), expected);
        let write_id = can_confirm.then(|| {
            let write_id = self.next_smartshift_write_id;
            self.next_smartshift_write_id = self.next_smartshift_write_id.saturating_add(1);
            self.smartshift_pending_confirm
                .insert(key.clone(), write_id);
            write_id
        });
        self.smartshift_write_status.insert(
            key,
            match write_id {
                Some(write_id) => SmartShiftWriteStatus::Applying { expected, write_id },
                None => SmartShiftWriteStatus::Failed,
            },
        );
    }

    /// Whether the active device's scroll wheel is inverted (issue #126).
    /// `false` when no device is selected or the device hasn't opted in.
    #[must_use]
    pub fn current_invert_scroll(&self) -> bool {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .is_some_and(|key| self.config.invert_scroll(key))
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
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — invert-scroll change ignored");
            return;
        };
        self.config.set_invert_scroll(&key, invert);
        self.persist_and_reload("invert scroll");
    }

    /// The active device's persisted wheel resolution, or `None` when OpenLogi
    /// leaves the device default untouched.
    #[must_use]
    pub fn current_scroll_resolution(&self) -> Option<openlogi_core::config::ScrollResolution> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.scroll_resolution(key))
    }

    /// Whether the active device exposes HID++ `0x2121 HiResWheel`.
    #[must_use]
    pub fn current_hires_wheel_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.hires_wheel)
    }

    /// Persist the active device's wheel resolution and ask the agent to reload
    /// it. `None` removes OpenLogi's override. No-op without a selected,
    /// HiResWheel-capable device.
    pub fn commit_scroll_resolution(
        &mut self,
        resolution: Option<openlogi_core::config::ScrollResolution>,
    ) {
        let Some((key, supported)) = self.current_record().and_then(|record| {
            let key = record.persistent_config_key()?.to_string();
            Some((
                key,
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.hires_wheel),
            ))
        }) else {
            debug!("no persistent device key — wheel-resolution change ignored");
            return;
        };
        if !set_scroll_resolution_if_supported(&mut self.config, &key, supported, resolution) {
            debug!("active device does not support HiResWheel");
            return;
        }
        self.persist_and_reload("wheel resolution");
    }

    /// Take the active device's pending SmartShift confirm, if any. Returns the
    /// `(config_key, route, write_id)` for a one-shot re-read that replaces the
    /// optimistic value with the device's real state; consumed once so it
    /// doesn't re-fire.
    pub fn take_active_smartshift_confirm(&mut self) -> Option<(String, DeviceRoute, u64)> {
        let record = self.current_record()?;
        let key = record.config_key.clone();
        let route = record.route.clone()?;
        self.smartshift_pending_confirm
            .remove(&key)
            .map(|write_id| (key, route, write_id))
    }

    /// Mark a post-write confirmation as failed when its reply channel closes.
    pub fn fail_smartshift_confirm(&mut self, key: &str, write_id: u64) {
        if matches!(
            self.smartshift_write_status.get(key),
            Some(SmartShiftWriteStatus::Applying {
                write_id: current,
                ..
            }) if *current == write_id
        ) {
            self.smartshift_write_status
                .insert(key.to_string(), SmartShiftWriteStatus::Failed);
        }
    }

    /// Onboard-profiles status for the active device.
    #[must_use]
    pub fn current_profiles_status(&self) -> ProfilesLoad {
        self.current_record()
            .map_or(ProfilesLoad::Unknown, |record| {
                self.profiles_data.status(&record.config_key)
            })
    }

    /// Whether the active device still needs an onboard-profiles read.
    #[must_use]
    pub fn current_profiles_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.profiles_data.unqueried(&record.config_key))
    }

    /// Mark onboard-profiles discovery as in flight for `key`.
    pub fn mark_profiles_loading(&mut self, key: &str) {
        self.profiles_data.mark_loading(key);
    }

    /// Reset a stuck `Loading` for `key` back to `Unknown` — called when the
    /// read worker vanished without delivering a result.
    pub fn clear_profiles_loading(&mut self, key: &str) {
        self.profiles_data.clear_loading(key);
    }

    /// Drop the active device's recorded onboard-profiles state so the next
    /// render re-runs discovery — the "click to retry" affordance.
    pub fn retry_active_profiles(&mut self) {
        if let Some(key) = self.current_record().map(|r| r.config_key.clone()) {
            self.profiles_data.retry(&key);
        }
    }

    /// Store an onboard-profiles read result, with the same stale-route guard
    /// and retry policy as [`Self::store_smartshift_status`].
    pub fn store_profiles_info(
        &mut self,
        key: String,
        route: &DeviceRoute,
        result: Result<OnboardProfilesInfo, WriteError>,
    ) {
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.config_key == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.config_key == key);
        self.profiles_data.store(
            key,
            result,
            profiles_error_is_permanent,
            matches_route,
            still_present,
            "onboard profiles",
        );
    }

    /// Write an onboard-profiles config to the active device (best-effort over
    /// IPC), persist it to `config.toml`, and optimistically update the cached
    /// state — the mode lives in device RAM and reverts to onboard on a power
    /// cycle, so the agent re-applies the persisted config on reconnect.
    /// No-op when no device is selected.
    pub fn commit_onboard_profiles(&mut self, mode: ProfilesMode, profile: Option<u16>) {
        let Some(record) = self.current_record() else {
            debug!("no active device — onboard-profiles change ignored");
            return;
        };
        let key = record.config_key.clone();
        let route = record.route.clone();
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetOnboardProfiles(
                route, mode, profile,
            ));
        }
        self.config.set_onboard_profiles(
            &key,
            openlogi_core::config::OnboardProfiles {
                mode: match mode {
                    ProfilesMode::Host => openlogi_core::config::ProfileSource::Host,
                    ProfilesMode::Onboard => openlogi_core::config::ProfileSource::Onboard,
                },
                profile,
            },
        );
        self.persist_and_reload("onboard profiles");
        // Reflect the write immediately (when a read already resolved) so the
        // panel doesn't flicker back, and queue a confirming re-read to replace
        // the optimistic value with the device's actual state.
        if let Some(ProfilesLoad::Ready(info)) = self.profiles_data.get(&key) {
            let mut info = info.clone();
            info.mode = mode;
            if let Some(sector) = profile {
                info.active_profile = sector;
            }
            self.profiles_data.set_ready(key.clone(), info);
        }
        self.profiles_pending_confirm.insert(key);
    }

    /// Take the active device's pending onboard-profiles confirm, if any — the
    /// one-shot re-read companion to [`Self::commit_onboard_profiles`].
    pub fn take_active_profiles_confirm(&mut self) -> Option<(String, DeviceRoute)> {
        let record = self.current_record()?;
        let key = record.config_key.clone();
        let route = record.route.clone()?;
        self.profiles_pending_confirm
            .remove(&key)
            .then_some((key, route))
    }

    /// The lighting config for the active device, or the default when none is
    /// stored / no device is selected.
    #[must_use]
    pub fn lighting(&self) -> Lighting {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.lighting(key))
            .unwrap_or_default()
    }

    /// The stored lighting config for `key`, or `None` when unset.
    #[must_use]
    pub fn lighting_for(&self, key: &str) -> Option<Lighting> {
        if PhysicalDeviceKey::is_transient(key)
            || self
                .device_list
                .iter()
                .any(|record| record.config_key == key && !record.is_persistent())
        {
            return None;
        }
        self.config.lighting(key)
    }

    /// Persist a new lighting config for the active device and push it to the
    /// hardware (best-effort). No-op when no device is selected.
    pub fn commit_lighting(&mut self, lighting: Lighting) {
        let Some(record) = self.current_record() else {
            debug!("no active device — lighting change ignored");
            return;
        };
        let key = record.persistent_config_key().map(str::to_string);
        let target = record.route.clone();
        if let Some(route) = target {
            self.send_ipc(crate::ipc_client::Command::SetLighting(
                route,
                lighting.clone(),
            ));
        }
        let Some(key) = key else {
            debug!("transient device lighting applied without persistence");
            return;
        };
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
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        if let Some(route) = route {
            self.send_ipc(crate::ipc_client::Command::SetDpi(route, dpi));
        }
        if let Some(persistent_key) = persistent_key {
            self.config.set_dpi(&persistent_key, dpi);
            self.persist_and_reload("DPI");
        } else {
            debug!(key, "transient device DPI applied without persistence");
        }
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
    /// The callers are the menu-bar / notification-area toggle in Settings,
    /// shown only where there's a tray (macOS + Windows), so the setter is
    /// gated the same way to stay dead-code-clean on Linux.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
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

    /// Persist the preferred device-asset source. The Settings view requests a
    /// refresh separately when automatic downloads are enabled, so this setter
    /// remains side-effect-free beyond configuration I/O.
    pub fn set_asset_source(&mut self, source: AssetSourcePreference) {
        if self.config.app_settings.asset_source == source {
            return;
        }
        self.config.app_settings.asset_source = source;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist asset-source setting");
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

        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(
                ?button,
                "no persistent device key — binding kept in memory only"
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
            self.current_record()
                .and_then(DeviceRecord::persistent_config_key),
            self.current_app_bundle.as_deref(),
        )
    }

    fn gesture_bindings_for_current(&self) -> BTreeMap<GestureDirection, Action> {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
        else {
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
        let key = self.current_record()?.persistent_config_key()?;
        self.config.gesture_owner(key)
    }

    /// Make `button` the current device's gesture button (or clear it with
    /// `None`), enforcing the one-gesture-button-per-device lock. Persists, tells
    /// the agent to rebuild, and refreshes the projected maps the UI reads.
    pub fn commit_gesture_owner(&mut self, button: Option<ButtonId>) {
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
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
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(
                ?direction,
                "no persistent device key — gesture binding edit ignored"
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
        let Some(config_key) = record.persistent_config_key() else {
            continue;
        };
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
        if config.device_identity(config_key) != Some(&identity) {
            config.set_device_identity(config_key, identity);
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

fn smartshift_write_outcome(
    expected: SmartShiftStatus,
    load: Option<&SmartShiftLoad>,
) -> Option<SmartShiftWriteStatus> {
    match load {
        Some(SmartShiftLoad::Ready(actual)) if *actual == expected => {
            Some(SmartShiftWriteStatus::Confirmed)
        }
        Some(SmartShiftLoad::Ready(_)) => Some(SmartShiftWriteStatus::Failed),
        Some(SmartShiftLoad::Failed(_) | SmartShiftLoad::Unsupported(_)) => {
            Some(SmartShiftWriteStatus::Failed)
        }
        None | Some(SmartShiftLoad::Unknown | SmartShiftLoad::Loading) => None,
    }
}

fn smartshift_read_is_current(
    read_id: Option<u64>,
    write_status: Option<&SmartShiftWriteStatus>,
) -> bool {
    match (read_id, write_status) {
        (
            Some(read_id),
            Some(SmartShiftWriteStatus::Applying {
                write_id: current, ..
            }),
        ) => read_id == *current,
        (None, Some(SmartShiftWriteStatus::Applying { .. })) | (Some(_), _) => false,
        (None, _) => true,
    }
}

/// Whether an onboard-profiles read error is permanent: the device genuinely
/// lacks `0x8100`. Everything else (timeouts, busy device) is transient.
fn profiles_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

fn set_scroll_resolution_if_supported(
    config: &mut Config,
    key: &str,
    supported: bool,
    resolution: Option<openlogi_core::config::ScrollResolution>,
) -> bool {
    if !supported {
        return false;
    }
    config.set_scroll_resolution(key, resolution);
    true
}

impl Global for AppState {}

#[cfg(test)]
mod tests {
    use openlogi_core::config::{Config, DeviceIdentity, Lighting, ScrollResolution};
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports, PairedDevice,
        ReceiverInfo,
    };

    use crate::asset::AssetResolver;

    use openlogi_hid::{SmartShiftMode, SmartShiftStatus};

    use super::{
        AppState, Load, SmartShiftWriteStatus, build_device_list,
        set_scroll_resolution_if_supported, smartshift_read_is_current, smartshift_write_outcome,
    };

    fn direct_inventory(unit_id: [u8; 4]) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "MX Master 3S".to_string(),
                vendor_id: 0x046d,
                product_id: 0xb023,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: openlogi_hid::DIRECT_DEVICE_INDEX,
                codename: Some("MX Master 3S".to_string()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: Some(DeviceModelInfo {
                    entity_count: 1,
                    serial_number: None,
                    unit_id,
                    transports: DeviceTransports::default(),
                    model_ids: [0xb034, 0, 0],
                    extended_model_id: 2,
                }),
                capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
            }],
        }
    }

    #[test]
    fn transient_identity_is_not_persisted_or_retained_after_resolution() {
        let cache = AssetResolver::new();
        let transient_inventory = direct_inventory([0; 4]);
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state =
            AppState::with_runtime(Config::default(), &[transient_inventory], &cache, commands);
        let transient_key = "direct:046d:b023:unit:00000000";

        assert_eq!(state.device_list.len(), 1);
        assert!(state.config.device_identity(transient_key).is_none());
        state.commit_dpi(2400);
        assert!(state.config.dpi(transient_key).is_none());

        let stable_list = build_device_list(
            &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
            &cache,
            &state.config,
        );
        let merged = state.merge_inventory_snapshot(stable_list);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].config_key, "direct:046d:b023:unit:a393cae0");
        assert!(merged[0].is_persistent());
    }

    #[test]
    fn transient_probe_folds_into_its_known_card() {
        // #482: a half-read probe (all-zero unit id) of the only known device
        // with that vid/pid must not evict the known card or appear beside it —
        // the card keeps its identity and takes the live volatile state.
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            Config::default(),
            &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
            &cache,
            commands,
        );
        let stable_key = "direct:046d:b023:unit:a393cae0";
        assert_eq!(state.device_list[0].config_key, stable_key);

        let transient_list = build_device_list(&[direct_inventory([0; 4])], &cache, &state.config);
        let merged = state.merge_inventory_snapshot(transient_list);

        assert_eq!(merged.len(), 1, "no second card for the half-read probe");
        assert_eq!(merged[0].config_key, stable_key);
        assert!(merged[0].is_persistent());
        assert!(merged[0].online, "the live probe supplies volatile state");
        assert!(merged[0].route.is_some(), "the live route is kept usable");
    }

    #[test]
    fn transient_record_beside_its_live_device_is_dropped() {
        // Both a full and a half-read probe of the same wire product in one
        // snapshot: the transient record is probe noise, not a second device.
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            Config::default(),
            &[direct_inventory([0xa3, 0x93, 0xca, 0xe0])],
            &cache,
            commands,
        );

        let both = build_device_list(
            &[
                direct_inventory([0xa3, 0x93, 0xca, 0xe0]),
                direct_inventory([0; 4]),
            ],
            &cache,
            &state.config,
        );
        assert_eq!(both.len(), 2);
        let merged = state.merge_inventory_snapshot(both);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].config_key, "direct:046d:b023:unit:a393cae0");
        assert!(merged[0].online);
    }

    #[test]
    fn transient_probe_adopts_the_absent_sibling_of_a_live_twin() {
        // Two same-model devices; one probes complete, the other half-reads.
        // The live twin must not get the transient discarded as its own noise:
        // the half-read probe can only be the sibling, which keeps its card
        // online and routed.
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            Config::default(),
            &[
                direct_inventory([1, 1, 1, 1]),
                direct_inventory([2, 2, 2, 2]),
            ],
            &cache,
            commands,
        );

        let snapshot = build_device_list(
            &[direct_inventory([1, 1, 1, 1]), direct_inventory([0; 4])],
            &cache,
            &state.config,
        );
        let merged = state.merge_inventory_snapshot(snapshot);

        assert_eq!(merged.len(), 2, "no third card for the half-read probe");
        let Some(sibling) = merged
            .iter()
            .find(|r| r.config_key == "direct:046d:b023:unit:02020202")
        else {
            panic!("the sibling card must survive under its physical key");
        };
        assert!(
            sibling.online,
            "the half-read probe keeps the sibling online"
        );
        assert!(sibling.route.is_some(), "the live route stays usable");
    }

    #[test]
    fn ambiguous_transient_probe_is_not_adopted() {
        // Two same-model devices are known; a half-read probe could be either,
        // so neither card may steal it.
        let cache = AssetResolver::new();
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = AppState::with_runtime(
            Config::default(),
            &[
                direct_inventory([1, 1, 1, 1]),
                direct_inventory([2, 2, 2, 2]),
            ],
            &cache,
            commands,
        );
        assert_eq!(state.device_list.len(), 2);

        let transient_list = build_device_list(&[direct_inventory([0; 4])], &cache, &state.config);
        let merged = state.merge_inventory_snapshot(transient_list);

        assert_eq!(merged.len(), 3, "both known cards survive on grace");
        assert_eq!(
            merged.iter().filter(|r| !r.is_persistent()).count(),
            1,
            "the transient card stays its own record"
        );
    }

    #[test]
    fn historical_transient_lighting_is_not_exposed_without_a_live_record() {
        let transient_key = "direct:046d:b023:unit:00000000";
        let mut config = Config::default();
        config.set_lighting(transient_key, Lighting::default());
        assert!(config.lighting(transient_key).is_some());
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::with_runtime(config, &[], &AssetResolver::new(), commands);

        assert!(state.device_list.is_empty());
        assert!(state.lighting_for(transient_key).is_none());
    }

    #[test]
    fn smartshift_write_feedback_requires_the_written_value() {
        let expected = SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: 12,
            tunable_torque: 0,
        };
        assert_eq!(smartshift_write_outcome(expected, None), None);
        assert_eq!(
            smartshift_write_outcome(expected, Some(&Load::Ready(expected))),
            Some(SmartShiftWriteStatus::Confirmed)
        );
        assert_eq!(
            smartshift_write_outcome(
                expected,
                Some(&Load::Ready(SmartShiftStatus {
                    auto_disengage: 13,
                    ..expected
                })),
            ),
            Some(SmartShiftWriteStatus::Failed)
        );
        assert_eq!(
            smartshift_write_outcome(
                expected,
                Some(&Load::<SmartShiftStatus>::Failed("timeout".to_string(),))
            ),
            Some(SmartShiftWriteStatus::Failed)
        );
    }

    #[test]
    fn stale_smartshift_reads_do_not_resolve_newer_writes() {
        let expected = SmartShiftStatus {
            mode: SmartShiftMode::Ratchet,
            auto_disengage: 12,
            tunable_torque: 0,
        };
        let applying = SmartShiftWriteStatus::Applying {
            expected,
            write_id: 2,
        };

        assert!(smartshift_read_is_current(Some(2), Some(&applying)));
        assert!(!smartshift_read_is_current(Some(1), Some(&applying)));
        assert!(!smartshift_read_is_current(None, Some(&applying)));
        assert!(!smartshift_read_is_current(
            Some(2),
            Some(&SmartShiftWriteStatus::Confirmed)
        ));
        assert!(smartshift_read_is_current(None, None));
    }

    #[test]
    fn known_offline_device_is_an_asset_sync_target() {
        let model = DeviceModelInfo {
            entity_count: 0,
            serial_number: None,
            unit_id: [0; 4],
            transports: DeviceTransports::default(),
            model_ids: [0xb034, 0, 0],
            extended_model_id: 2,
        };
        let mut config = Config::default();
        config.set_device_identity(
            "2b034",
            DeviceIdentity {
                display_name: "MX Anywhere 3S".to_string(),
                kind: DeviceKind::Mouse,
                capabilities: Capabilities::presumed_from_kind(DeviceKind::Mouse),
                model_info: Some(model.clone()),
                codename: Some("MX Anywhere 3S".to_string()),
            },
        );
        let (commands, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::with_runtime(config, &[], &AssetResolver::new(), commands);

        assert_eq!(
            state.asset_models(),
            vec![(model, Some("MX Anywhere 3S".to_string()))]
        );
    }

    #[test]
    fn gui_state_saves_and_clears_supported_wheel_resolution() {
        let mut config = Config::default();
        assert!(set_scroll_resolution_if_supported(
            &mut config,
            "mouse",
            true,
            Some(ScrollResolution::Low),
        ));
        assert_eq!(
            config.scroll_resolution("mouse"),
            Some(ScrollResolution::Low)
        );

        assert!(set_scroll_resolution_if_supported(
            &mut config,
            "mouse",
            true,
            None,
        ));
        assert_eq!(config.scroll_resolution("mouse"), None);
    }

    #[test]
    fn gui_state_ignores_unsupported_wheel_resolution() {
        let mut config = Config::default();
        assert!(!set_scroll_resolution_if_supported(
            &mut config,
            "mouse",
            false,
            Some(ScrollResolution::High),
        ));
        assert_eq!(config.scroll_resolution("mouse"), None);
    }
}
