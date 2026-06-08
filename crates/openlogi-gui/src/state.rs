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

#![allow(
    dead_code,
    reason = "fields are read once their owning component lands in UI.md phases 2–4"
)]

use std::collections::BTreeMap;

use gpui::{App, Global};
use openlogi_core::config::{AppSettings, Config, Lighting};
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{
    DeviceRoute, DpiCapabilities, DpiInfo, SmartShiftMode, SmartShiftStatus, WriteError,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};

mod devices;

pub use devices::DeviceRecord;
pub use openlogi_agent_core::DpiCycleState;

use crate::asset::AssetResolver;
use crate::data::mouse_buttons::{Action, Binding, ButtonId, GestureDirection};
use crate::state::devices::{build_device_list, pick_initial_device, sort_device_list};
use openlogi_agent_core::bindings::{bindings_for, gesture_bindings_for};

/// Default DPI value applied to a fresh AppState. Matches a common Logitech
/// mid-range mouse and keeps the dot-preview visually obvious from frame one.
pub const DEFAULT_DPI: u32 = 1600;

/// Inventory snapshots can briefly miss a real device while another HID++
/// request is in flight. Keep the previous record through this many
/// consecutive misses so a transient probe timeout does not make the carousel
/// disappear mid-interaction.
const INVENTORY_MISS_GRACE: u8 = 2;

/// How many times to retry DPI capability discovery after a transient HID++
/// error (read timeout, busy device) before marking the device unsupported. A
/// genuine "feature not supported" reply is permanent and never retried.
const DPI_LOAD_MAX_ATTEMPTS: u8 = 3;

/// Per-device DPI capability loading state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DpiStatus {
    /// The selected device has not been queried yet.
    Unknown,
    /// A background HID++ read is in flight.
    Loading,
    /// The device reported its current DPI and supported values.
    Ready(DpiInfo),
    /// Transient discovery errors (read timeouts, busy device) exhausted the
    /// retry budget. Distinct from [`Self::Unsupported`] because the device may
    /// well support DPI — re-selecting it (see [`AppState::set_current_device`])
    /// grants a fresh attempt.
    Failed(String),
    /// The device genuinely does not support the AdjustableDpi feature; never
    /// retried.
    Unsupported(String),
}

/// Per-device SmartShift (`0x2111`) loading state. Mirrors [`DpiStatus`]:
/// lazily loaded because the HID++ read must not block device switching or
/// rendering. Unlike DPI presets, the resolved config is *not* persisted to
/// `config.toml` — the device stores wheel mode / threshold / torque in its
/// own non-volatile memory, so the GUI only ever reads and writes the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmartShiftLoad {
    /// The selected device has not been queried yet.
    Unknown,
    /// A background HID++ read is in flight.
    Loading,
    /// The device reported its current SmartShift configuration.
    Ready(SmartShiftStatus),
    /// Transient read errors exhausted the retry budget; retryable on
    /// re-select.
    Failed(String),
    /// The device genuinely does not expose SmartShift (`0x2111`); never
    /// retried.
    Unsupported(String),
}

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
    /// Whether the process holds macOS Accessibility permission. Drives the
    /// permission gate; flipped by the accessibility watcher when the user
    /// grants access. Always `true` on platforms without the concept.
    pub accessibility_granted: bool,
    /// Whether the first device enumeration is still in flight. Startup no
    /// longer blocks on enumeration (see `main`); this drives the "Scanning…"
    /// vs "No devices connected" empty state and is cleared once the inventory
    /// watcher delivers its first snapshot.
    pub scanning: bool,
    /// Bindings for the *currently selected* device. Reloaded whenever the
    /// carousel selection changes.
    pub button_bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction sub-bindings for the gesture button on the currently
    /// selected device. Edited via the gesture picker; persistence shape
    /// lives in [`openlogi_core::config::DeviceConfig::gesture_bindings`].
    pub gesture_bindings: BTreeMap<GestureDirection, Action>,
    pub dpi: u32,
    /// DPI capability state keyed by [`DeviceRecord::config_key`]. Loaded
    /// lazily because HID++ reads must not block device switching or rendering.
    pub dpi_by_device: BTreeMap<String, DpiStatus>,
    /// Consecutive inventory snapshots that omitted a previously-known device,
    /// keyed by [`DeviceRecord::config_key`]. Used to debounce transient HID++
    /// probe misses without hiding a real disconnect forever.
    inventory_misses: BTreeMap<String, u8>,
    /// Consecutive failed DPI discovery attempts, keyed by
    /// [`DeviceRecord::config_key`]. Lets a transient read error retry a few
    /// times (see [`DPI_LOAD_MAX_ATTEMPTS`]) instead of sticking the device on
    /// [`DpiStatus::Unsupported`] forever.
    dpi_load_attempts: BTreeMap<String, u8>,
    /// SmartShift (`0x2111`) configuration state keyed by
    /// [`DeviceRecord::config_key`]. Loaded lazily on the same pattern as
    /// [`Self::dpi_by_device`]; the device persists the values itself, so this
    /// is a read/write cache, not a source of truth saved to disk. Private so
    /// all access goes through the accessor methods below (`current_smartshift_*`,
    /// `store_smartshift_status`), which enforce the stale-result and retry rules.
    smartshift_by_device: BTreeMap<String, SmartShiftLoad>,
    /// Consecutive failed SmartShift read attempts, keyed by
    /// [`DeviceRecord::config_key`] — mirrors [`Self::dpi_load_attempts`].
    smartshift_load_attempts: BTreeMap<String, u8>,
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
}

impl AppState {
    /// Build the global from a loaded config + enumerated inventories.
    ///
    /// The initial selection prefers [`Config::selected_device`] if it still
    /// matches one of the paired devices; otherwise it falls back to index 0.
    ///
    /// A fresh `Arc<RwLock<…>>` is created for [`Self::hook_bindings`]. When
    /// the OS event hook (P0.1) needs to share the same map, the caller
    /// builds the `Arc` first and uses [`Self::with_runtime_shared`] instead.
    #[must_use]
    pub fn with_runtime(
        config: Config,
        inventories: &[DeviceInventory],
        cache: &AssetResolver,
        ipc_commands: mpsc::UnboundedSender<crate::ipc_client::Command>,
    ) -> Self {
        let device_list = build_device_list(inventories, cache);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let mut state = Self {
            current_device,
            current_app_bundle: None,
            active_button: None,
            // Updated from the agent's IPC `status` poll; the GUI no longer runs
            // the hook, so it can't meaningfully query Accessibility itself.
            accessibility_granted: false,
            scanning: true,
            button_bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            dpi: DEFAULT_DPI,
            dpi_by_device: BTreeMap::new(),
            inventory_misses: BTreeMap::new(),
            dpi_load_attempts: BTreeMap::new(),
            smartshift_by_device: BTreeMap::new(),
            smartshift_load_attempts: BTreeMap::new(),
            smartshift_pending_confirm: std::collections::BTreeSet::new(),
            device_list,
            config,
            ipc_commands,
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

    /// A clone of the IPC command sender, so views (the DPI / SmartShift panels)
    /// can issue device reads and writes through the agent themselves.
    #[must_use]
    pub fn ipc_sender(&self) -> mpsc::UnboundedSender<crate::ipc_client::Command> {
        self.ipc_commands.clone()
    }

    /// Ask the agent to fire the macOS Accessibility prompt. The agent owns the
    /// CGEventTap, so the system dialog must name and authorize the *agent*
    /// binary; prompting in the GUI process (as the pre-split build did) would
    /// grant the wrong binary and the hook would never install.
    pub fn request_accessibility_prompt(&self) {
        self.send_ipc(crate::ipc_client::Command::RequestAccessibilityPrompt);
    }

    /// Build the button-binding, gesture-binding, and DPI snapshots consumed by
    /// the OS hook and gesture watcher before the GPUI global exists. Uses the
    /// same device-selection and binding rules as [`Self::with_runtime_shared`].
    #[must_use]
    pub fn initial_hook_state(
        config: &Config,
        inventories: &[DeviceInventory],
        cache: &AssetResolver,
    ) -> (
        BTreeMap<ButtonId, Action>,
        BTreeMap<GestureDirection, Action>,
        DpiCycleState,
    ) {
        let device_list = build_device_list(inventories, cache);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let record = device_list.get(current_device);
        let config_key = record.map(|r| r.config_key.as_str());
        let bindings = bindings_for(config, config_key, None);
        let gesture_bindings = gesture_bindings_for(config, config_key);
        let presets = record
            .map(|r| config.dpi_presets(&r.config_key))
            .unwrap_or_default();
        let target = record.and_then(|r| r.route.clone());
        (
            bindings,
            gesture_bindings,
            DpiCycleState {
                presets,
                index: 0,
                target,
                capabilities: None,
            },
        )
    }

    /// Update the frontmost-app tracking + reload the binding map to overlay
    /// any per-app overrides for the new app (P1.4). Hook-shared `Arc` gets
    /// the same map so background button presses observe the new bindings
    /// immediately.
    ///
    /// No-op when `bundle` matches the current value.
    pub fn set_current_app(&mut self, bundle: Option<String>) {
        if bundle == self.current_app_bundle {
            return;
        }
        debug!(?bundle, "foreground app changed");
        self.current_app_bundle = bundle;
        self.button_bindings = self.bindings_for_current();
    }

    /// The active device, or `None` when [`Self::device_list`] is empty or
    /// `current_device` is past the end.
    #[must_use]
    pub fn current_record(&self) -> Option<&DeviceRecord> {
        self.device_list.get(self.current_device)
    }

    /// Replace [`Self::device_list`] from a fresh inventory snapshot,
    /// preserving the carousel selection by `config_key` when possible. If
    /// the previously-selected device disappeared, the selection falls back
    /// to index 0.
    ///
    /// No-op when the new list has the same `config_key` sequence as the
    /// current one — avoids spurious `observe_global` notifications during
    /// quiet polling cycles (P1.6).
    pub fn refresh_inventories(&mut self, inventories: &[DeviceInventory], cache: &AssetResolver) {
        let new_list = build_device_list(inventories, cache);
        let merged_list = self.merge_inventory_snapshot(new_list);
        // Compare routes too, not just config_key: a device can reconnect on a
        // new HID++ index while keeping its model-derived config_key, and the
        // fresh route must replace the stale one so reads/writes don't target a
        // dead index.
        let unchanged = merged_list.len() == self.device_list.len()
            && merged_list
                .iter()
                .zip(self.device_list.iter())
                .all(|(a, b)| a.config_key == b.config_key && a.route == b.route);
        if unchanged {
            return;
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
            self.dpi_by_device.remove(key);
            self.dpi_load_attempts.remove(key);
            self.smartshift_by_device.remove(key);
            self.smartshift_load_attempts.remove(key);
        }
        self.dpi_by_device
            .retain(|key, _| self.device_list.iter().any(|r| r.config_key == *key));
        self.dpi_load_attempts
            .retain(|key, _| self.device_list.iter().any(|r| r.config_key == *key));
        self.smartshift_by_device
            .retain(|key, _| self.device_list.iter().any(|r| r.config_key == *key));
        self.smartshift_load_attempts
            .retain(|key, _| self.device_list.iter().any(|r| r.config_key == *key));
        self.current_device = new_index;
        // The active device may have changed (selection fell back to index 0
        // when the previous one vanished); re-seed the displayed DPI so it
        // tracks the now-current device rather than the old one.
        self.dpi = self.dpi_for_current();
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        // Display state only — the agent runs its own inventory watcher and
        // rebuilds the live binding/DPI maps itself.
    }

    fn merge_inventory_snapshot(&mut self, new_list: Vec<DeviceRecord>) -> Vec<DeviceRecord> {
        let mut by_key = new_list
            .into_iter()
            .map(|record| (record.config_key.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let mut merged = Vec::with_capacity(by_key.len().max(self.device_list.len()));

        for previous in &self.device_list {
            if let Some(mut record) = by_key.remove(&previous.config_key) {
                self.inventory_misses.remove(&previous.config_key);
                // Capabilities can only be measured while the device is online.
                // When a known device goes offline (or a probe fails) the fresh
                // record carries `None`; keep the last-known set so a sleeping
                // mouse doesn't lose its button/pointer panels.
                if record.capabilities.is_none() {
                    record.capabilities = previous.capabilities;
                }
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
            if matches!(self.dpi_by_device.get(&key), Some(DpiStatus::Failed(_))) {
                self.dpi_by_device.remove(&key);
                self.dpi_load_attempts.remove(&key);
            }
            if matches!(
                self.smartshift_by_device.get(&key),
                Some(SmartShiftLoad::Failed(_))
            ) {
                self.smartshift_by_device.remove(&key);
                self.smartshift_load_attempts.remove(&key);
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist selected device");
        }
        // The agent owns the hook + device I/O; have it switch devices too.
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist DPI presets to config.toml");
        }
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
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
        self.current_record()
            .and_then(|record| self.dpi_by_device.get(&record.config_key).cloned())
            .unwrap_or(DpiStatus::Unknown)
    }

    /// Whether the active device still needs a DPI read (no status recorded —
    /// i.e. `Unknown`). Cheaper than `current_dpi_status() == Unknown`: it
    /// avoids cloning the `DpiInfo`, which matters on the per-frame render path.
    #[must_use]
    pub fn current_dpi_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| !self.dpi_by_device.contains_key(&record.config_key))
    }

    /// The active device's known DPI, falling back to [`DEFAULT_DPI`] until its
    /// capability read completes. Used to seed `self.dpi` on a device switch.
    #[must_use]
    fn dpi_for_current(&self) -> u32 {
        self.current_record()
            .and_then(|record| self.dpi_by_device.get(&record.config_key))
            .and_then(|status| match status {
                DpiStatus::Ready(info) => Some(u32::from(info.current)),
                _ => None,
            })
            .unwrap_or(DEFAULT_DPI)
    }

    /// Mark DPI capability discovery as in flight for `key`.
    pub fn mark_dpi_loading(&mut self, key: &str) {
        self.dpi_by_device
            .insert(key.to_string(), DpiStatus::Loading);
    }

    /// Reset a stuck `Loading` for `key` back to `Unknown`. Called when the
    /// discovery worker vanished without delivering a result (e.g. it panicked),
    /// so the device isn't wedged on "Reading…" with no path to retry.
    pub fn clear_dpi_loading(&mut self, key: &str) {
        if matches!(self.dpi_by_device.get(key), Some(DpiStatus::Loading)) {
            self.dpi_by_device.remove(key);
        }
    }

    /// Drop the active device's recorded DPI status so the next render
    /// re-runs discovery. Backs the "click to retry" affordance on a
    /// [`DpiStatus::Failed`] device, which is the only recovery path when the
    /// carousel has a single device (re-selecting it is a no-op).
    pub fn retry_active_dpi(&mut self) {
        if let Some(key) = self.current_record().map(|r| r.config_key.clone()) {
            self.dpi_by_device.remove(&key);
            self.dpi_load_attempts.remove(&key);
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
        let still_matches = self
            .device_list
            .iter()
            .any(|record| record.config_key == key && record.route.as_ref() == Some(route));
        if !still_matches {
            debug!(key, ?route, "stale DPI capability result ignored");
            // If the device is still present but on a different route (it
            // reconnected mid-read), drop the orphaned `Loading` marker so the
            // next render re-discovers against the live route instead of
            // spinning on "Reading…" forever.
            if self
                .device_list
                .iter()
                .any(|record| record.config_key == key)
            {
                self.dpi_by_device.remove(&key);
            }
            return;
        }

        let is_active = self.current_record().map(|r| r.config_key.as_str()) == Some(key.as_str());
        let status = match result {
            Ok(info) => {
                // Only the active device owns the shared `self.dpi`; a result
                // landing for a background device after a carousel switch must
                // not clobber the visible value.
                if is_active {
                    self.dpi = u32::from(info.current);
                }
                self.dpi_load_attempts.remove(&key);
                DpiStatus::Ready(info)
            }
            // A genuine "feature not supported" reply will never change — record
            // it and stop probing.
            Err(error) if dpi_error_is_permanent(&error) => {
                self.dpi_load_attempts.remove(&key);
                DpiStatus::Unsupported(error.to_string())
            }
            // Timeouts and other transient failures get a few more tries: clear
            // the status back to `Unknown` so the next render re-triggers the
            // read, until the attempt budget runs out, then settle on `Failed`
            // (retryable on re-select) rather than the permanent `Unsupported`.
            Err(error) => {
                let attempts = self.dpi_load_attempts.entry(key.clone()).or_insert(0);
                *attempts = attempts.saturating_add(1);
                if *attempts < DPI_LOAD_MAX_ATTEMPTS {
                    debug!(
                        key,
                        attempts = *attempts,
                        error = %error,
                        "transient DPI read error — will retry"
                    );
                    self.dpi_by_device.remove(&key);
                    return;
                }
                self.dpi_load_attempts.remove(&key);
                DpiStatus::Failed(error.to_string())
            }
        };
        self.dpi_by_device.insert(key, status);
    }

    /// DPI capabilities for the active device, if discovery succeeded.
    #[must_use]
    pub fn active_dpi_capabilities(&self) -> Option<&DpiCapabilities> {
        self.current_record()
            .and_then(|record| self.dpi_by_device.get(&record.config_key))
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
            .and_then(|record| self.smartshift_by_device.get(&record.config_key).cloned())
            .unwrap_or(SmartShiftLoad::Unknown)
    }

    /// Whether the active device still needs a SmartShift read (no status
    /// recorded). Cheaper than comparing a cloned [`SmartShiftLoad`] on the
    /// per-frame render path.
    #[must_use]
    pub fn current_smartshift_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| !self.smartshift_by_device.contains_key(&record.config_key))
    }

    /// The active device's resolved SmartShift config, if the read succeeded.
    /// Callers use it to preserve fields they don't mean to change (e.g.
    /// tunable torque) when writing back.
    #[must_use]
    pub fn current_smartshift_ready(&self) -> Option<SmartShiftStatus> {
        self.current_record()
            .and_then(|record| self.smartshift_by_device.get(&record.config_key))
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
        self.smartshift_by_device
            .insert(key.to_string(), SmartShiftLoad::Loading);
    }

    /// Reset a stuck `Loading` for `key` back to `Unknown` — called when the
    /// read worker vanished without delivering a result.
    pub fn clear_smartshift_loading(&mut self, key: &str) {
        if matches!(
            self.smartshift_by_device.get(key),
            Some(SmartShiftLoad::Loading)
        ) {
            self.smartshift_by_device.remove(key);
        }
    }

    /// Drop the active device's recorded SmartShift status so the next render
    /// re-runs discovery. Backs the "click to retry" affordance on a
    /// [`SmartShiftLoad::Failed`] device.
    pub fn retry_active_smartshift(&mut self) {
        if let Some(key) = self.current_record().map(|r| r.config_key.clone()) {
            self.smartshift_by_device.remove(&key);
            self.smartshift_load_attempts.remove(&key);
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
        let still_matches = self
            .device_list
            .iter()
            .any(|record| record.config_key == key && record.route.as_ref() == Some(route));
        if !still_matches {
            debug!(key, ?route, "stale SmartShift result ignored");
            if self.device_list.iter().any(|r| r.config_key == key) {
                self.smartshift_by_device.remove(&key);
            }
            return;
        }

        let status = match result {
            Ok(status) => {
                self.smartshift_load_attempts.remove(&key);
                SmartShiftLoad::Ready(status)
            }
            Err(error) if smartshift_error_is_permanent(&error) => {
                self.smartshift_load_attempts.remove(&key);
                SmartShiftLoad::Unsupported(error.to_string())
            }
            Err(error) => {
                let attempts = self
                    .smartshift_load_attempts
                    .entry(key.clone())
                    .or_insert(0);
                *attempts = attempts.saturating_add(1);
                if *attempts < DPI_LOAD_MAX_ATTEMPTS {
                    debug!(
                        key,
                        attempts = *attempts,
                        error = %error,
                        "transient SmartShift read error — will retry"
                    );
                    self.smartshift_by_device.remove(&key);
                    return;
                }
                self.smartshift_load_attempts.remove(&key);
                SmartShiftLoad::Failed(error.to_string())
            }
        };
        self.smartshift_by_device.insert(key, status);
    }

    /// Write a full SmartShift configuration to the active device (best-effort,
    /// on a background thread) and optimistically cache it. The device persists
    /// the values in its own NVM, so nothing is written to `config.toml`.
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
        // Reflect the write immediately so the panel doesn't flicker back to
        // the previous value before a re-read lands, but queue a confirming
        // re-read: the write is fire-and-forget, so a sleeping device that
        // rejected or timed it out would otherwise leave this optimistic value
        // showing as "applied" forever (Ready blocks any further read).
        self.smartshift_by_device.insert(
            key.clone(),
            SmartShiftLoad::Ready(SmartShiftStatus {
                mode,
                auto_disengage,
                tunable_torque,
            }),
        );
        self.smartshift_pending_confirm.insert(key);
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist lighting to config.toml");
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist launch-at-login setting");
        }
        // The agent owns autostart now; it reconciles its LaunchAgent (which
        // points at the agent, not the GUI) when it reloads the config.
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
    }

    /// Toggle the menu-bar (status item) icon preference and persist it. The
    /// icon is hosted by the always-on agent, which reads this on startup and
    /// installs the status item only when enabled — so the change takes effect
    /// the next time the agent launches (a no-restart live toggle would need a
    /// main-thread hop from the agent's IPC reload). `ReloadConfig` keeps the
    /// agent's other config in sync meanwhile. No-op when unchanged.
    pub fn set_show_in_menu_bar(&mut self, enabled: bool) {
        if self.config.app_settings.show_in_menu_bar == enabled {
            return;
        }
        self.config.app_settings.show_in_menu_bar = enabled;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist show-in-menu-bar setting");
        }
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist thumbwheel sensitivity");
        }
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
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
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist binding to config.toml");
        }
        // The agent owns the hook; have it rebuild its live map from config.
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
    }

    fn bindings_for_current(&self) -> BTreeMap<ButtonId, Action> {
        bindings_for(
            &self.config,
            self.current_record().map(|r| r.config_key.as_str()),
            self.current_app_bundle.as_deref(),
        )
    }

    fn gesture_bindings_for_current(&self) -> BTreeMap<GestureDirection, Action> {
        gesture_bindings_for(
            &self.config,
            self.current_record().map(|r| r.config_key.as_str()),
        )
    }

    /// Update a single gesture-button sub-binding in memory, on disk, and in the
    /// shared gesture map the watcher thread reads.
    pub fn commit_gesture_binding(&mut self, direction: GestureDirection, action: Action) {
        self.gesture_bindings.insert(direction, action.clone());

        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!(
                ?direction,
                "no active device key — gesture binding kept in memory only"
            );
            return;
        };
        self.config
            .set_gesture_direction(&key, ButtonId::GestureButton, direction, action);
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist gesture binding to config.toml");
        }
        // The agent owns the gesture watcher; have it rebuild from config.
        self.send_ipc(crate::ipc_client::Command::ReloadConfig);
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
