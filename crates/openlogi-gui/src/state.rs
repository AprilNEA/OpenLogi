//! App-wide UI state stored as a GPUI global.
//!
//! Anything that more than one view needs to read (current device, currently
//! armed button, the DPI value the panel and the dot-preview share) lives
//! here. Per-component scratch state (hover index, gesture point buffer) stays
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
use std::sync::{Arc, RwLock};

use gpui::Global;
use openlogi_core::config::{AppSettings, Config};
use openlogi_core::device::DeviceInventory;
use openlogi_hook::Hook;
use tracing::{debug, warn};

use crate::asset::{AssetResolver, ResolvedAsset};
use crate::components::dpi_panel::DpiTarget;
use crate::data::mouse_buttons::{
    Action, ButtonId, GestureDirection, default_binding, default_gesture_binding,
};

/// Default DPI value applied to a fresh AppState. Matches a common Logitech
/// mid-range mouse and keeps the dot-preview visually obvious from frame one.
pub const DEFAULT_DPI: u32 = 1600;

/// One paired device with everything the UI needs to switch to it in O(1):
/// the config key (for bindings/DPI persistence), a display name, the
/// resolved asset (PNG + metadata, or `None` for the synthetic fallback),
/// and the routing target for HID++ DPI writes.
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub config_key: String,
    pub display_name: String,
    pub asset: Option<ResolvedAsset>,
    pub dpi_target: Option<DpiTarget>,
}

/// Shared state consumed by the OS hook thread (P0.1) and the DPI panel UI
/// to implement [`Action::CycleDpiPresets`] / [`Action::SetDpiPreset`].
///
/// `index` is the position of the *current* DPI (i.e. the one last set on the
/// device), not the next-to-fire. `cycle` advances and returns the new value.
#[derive(Debug, Clone, Default)]
pub struct DpiCycleState {
    pub presets: Vec<u32>,
    pub index: usize,
    pub target: Option<DpiTarget>,
}

impl DpiCycleState {
    /// Advance to the next preset (wrapping last → first) and return the new
    /// DPI + the device target to write to. Returns `None` if `presets` is
    /// empty.
    pub fn cycle(&mut self) -> Option<(u32, Option<DpiTarget>)> {
        if self.presets.is_empty() {
            return None;
        }
        self.index = (self.index + 1) % self.presets.len();
        Some((self.presets[self.index], self.target.clone()))
    }

    /// Jump to preset `i`, clamping to the list length. Returns the DPI +
    /// target, or `None` if `presets` is empty.
    pub fn set(&mut self, i: usize) -> Option<(u32, Option<DpiTarget>)> {
        if self.presets.is_empty() {
            return None;
        }
        let clamped = i.min(self.presets.len() - 1);
        self.index = clamped;
        Some((self.presets[clamped], self.target.clone()))
    }
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
    /// Which gesture sub-direction is currently being edited inside the
    /// gesture-button popover. `None` means the popover is on the
    /// directions-list "page"; `Some(dir)` means it's drilled into the
    /// action picker for that direction. Pure UI scratch state — not
    /// persisted to disk.
    pub gesture_edit: Option<GestureDirection>,
    /// Whether the process holds macOS Accessibility permission. Drives the
    /// permission gate; flipped by the accessibility watcher when the user
    /// grants access. Always `true` on platforms without the concept.
    pub accessibility_granted: bool,
    /// Bindings for the *currently selected* device. Reloaded whenever the
    /// carousel selection changes.
    pub button_bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction sub-bindings for the gesture button on the currently
    /// selected device. Edited via the gesture picker; persistence shape
    /// lives in [`openlogi_core::config::DeviceConfig::gesture_bindings`].
    pub gesture_bindings: BTreeMap<GestureDirection, Action>,
    pub dpi: u32,
    /// All paired devices, in carousel order. Each entry caches the per-
    /// device data the views need so a switch is a pure index update.
    pub device_list: Vec<DeviceRecord>,
    /// Live config — kept in sync with disk via [`Self::commit_binding`] and
    /// [`Self::set_current_device`] so restarts preserve user bindings and
    /// the last-selected device.
    config: Config,
    /// Shared binding map consumed by the OS-level hook thread (P0.1). The
    /// hook holds the other `Arc` clone; writes here are picked up by the next
    /// hook callback without GPUI thread involvement.
    pub hook_bindings: Arc<RwLock<BTreeMap<ButtonId, Action>>>,
    /// Shared DPI-cycle state consumed by the hook thread when dispatching
    /// [`Action::CycleDpiPresets`] / [`Action::SetDpiPreset`].
    pub dpi_cycle: Arc<RwLock<DpiCycleState>>,
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
    ) -> Self {
        let bindings_arc = Arc::new(RwLock::new(BTreeMap::new()));
        let cycle_arc = Arc::new(RwLock::new(DpiCycleState::default()));
        Self::with_runtime_shared(config, inventories, cache, bindings_arc, cycle_arc)
    }

    /// Like [`Self::with_runtime`] but re-uses existing `Arc`s so the hook
    /// thread and `AppState` share the same maps. Both arcs are rewritten to
    /// match the resolved initial state so the hook sees correct values from
    /// the very first captured event.
    #[must_use]
    pub fn with_runtime_shared(
        config: Config,
        inventories: &[DeviceInventory],
        cache: &AssetResolver,
        hook_bindings: Arc<RwLock<BTreeMap<ButtonId, Action>>>,
        dpi_cycle: Arc<RwLock<DpiCycleState>>,
    ) -> Self {
        let device_list = build_device_list(inventories, cache);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let mut state = Self {
            current_device,
            current_app_bundle: None,
            active_button: None,
            gesture_edit: None,
            accessibility_granted: Hook::has_accessibility(),
            button_bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            dpi: DEFAULT_DPI,
            device_list,
            config,
            hook_bindings,
            dpi_cycle,
        };
        state.button_bindings = state.bindings_for_current();
        state.gesture_bindings = state.gesture_bindings_for_current();
        state.sync_hook_bindings();
        state.sync_dpi_cycle();
        state
    }

    /// Build the binding and DPI snapshots consumed by the OS hook before
    /// the GPUI global exists. Uses the same device-selection and binding
    /// overlay rules as [`Self::with_runtime_shared`].
    #[must_use]
    pub fn initial_hook_state(
        config: &Config,
        inventories: &[DeviceInventory],
        cache: &AssetResolver,
    ) -> (BTreeMap<ButtonId, Action>, DpiCycleState) {
        let device_list = build_device_list(inventories, cache);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let record = device_list.get(current_device);
        let bindings = bindings_for(config, record, None);
        let presets = record
            .map(|r| config.dpi_presets(&r.config_key))
            .unwrap_or_default();
        let target = record.and_then(|r| r.dpi_target.clone());
        (
            bindings,
            DpiCycleState {
                presets,
                index: 0,
                target,
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
        self.sync_hook_bindings();
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
        let unchanged = new_list.len() == self.device_list.len()
            && new_list
                .iter()
                .zip(self.device_list.iter())
                .all(|(a, b)| a.config_key == b.config_key);
        if unchanged {
            return;
        }

        let previous_key = self.current_record().map(|r| r.config_key.clone());
        let new_index = previous_key
            .as_deref()
            .and_then(|k| new_list.iter().position(|r| r.config_key == k))
            .unwrap_or(0);
        let connected_keys = new_list
            .iter()
            .map(|r| r.config_key.as_str())
            .collect::<Vec<_>>();
        debug!(
            count = new_list.len(),
            ?connected_keys,
            "inventory refreshed"
        );

        self.device_list = new_list;
        self.current_device = new_index;
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        self.sync_hook_bindings();
        self.sync_dpi_cycle();
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
        self.button_bindings = self.bindings_for_current();
        self.gesture_bindings = self.gesture_bindings_for_current();
        self.sync_hook_bindings();
        self.sync_dpi_cycle();
        let key = self.current_record().map(|r| r.config_key.clone());
        self.config.set_selected_device(key);
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist selected device");
        }
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
        self.sync_dpi_cycle();
    }

    /// Read the DPI preset list for the active device, or an empty `Vec`
    /// when no device is selected. UI helper.
    #[must_use]
    pub fn dpi_presets(&self) -> Vec<u32> {
        self.current_record()
            .map(|r| self.config.dpi_presets(&r.config_key))
            .unwrap_or_default()
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
        crate::launch_agent::reconcile(enabled);
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

    /// Update a single binding in memory, on disk, and in the shared hook
    /// map for the currently selected device.
    ///
    /// Disk failures and poisoned hook locks are logged at `warn` instead
    /// of bubbling up: the UI thread shouldn't crash because the user's
    /// home volume is full or because the hook thread panicked.
    pub fn commit_binding(&mut self, button: ButtonId, action: Action) {
        self.button_bindings.insert(button, action.clone());

        // Push into the hook-shared map. A poisoned lock means the hook
        // thread panicked; log and carry on rather than propagating to the
        // UI.
        match self.hook_bindings.write() {
            Ok(mut guard) => {
                guard.insert(button, action.clone());
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "hook_bindings lock poisoned — binding change will not reach the hook"
                );
            }
        }

        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!(
                ?button,
                "no active device key — binding kept in memory only"
            );
            return;
        };
        self.config.set_binding(&key, button, action);
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist binding to config.toml");
        }
    }

    fn bindings_for_current(&self) -> BTreeMap<ButtonId, Action> {
        bindings_for(
            &self.config,
            self.current_record(),
            self.current_app_bundle.as_deref(),
        )
    }

    fn gesture_bindings_for_current(&self) -> BTreeMap<GestureDirection, Action> {
        gesture_bindings_for(&self.config, self.current_record())
    }

    /// Update a single gesture-button sub-binding in memory + on disk for the
    /// currently selected device. The hook layer doesn't yet read the
    /// gesture map (P1.5), so this is config-only — no shared `Arc` push.
    pub fn commit_gesture_binding(&mut self, direction: GestureDirection, action: Action) {
        self.gesture_bindings.insert(direction, action.clone());

        let Some(key) = self.current_record().map(|r| r.config_key.clone()) else {
            debug!(
                ?direction,
                "no active device key — gesture binding kept in memory only"
            );
            return;
        };
        self.config.set_gesture_binding(&key, direction, action);
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist gesture binding to config.toml");
        }
    }

    /// Mirror [`Self::button_bindings`] into the hook-shared `Arc`. Called
    /// after the UI-side map changes wholesale (initial build, device
    /// switch) so the hook thread observes consistent state.
    fn sync_hook_bindings(&self) {
        match self.hook_bindings.write() {
            Ok(mut guard) => {
                *guard = self.button_bindings.clone();
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "hook_bindings lock poisoned — hook will keep stale bindings"
                );
            }
        }
    }

    /// Rebuild [`Self::dpi_cycle`] from the active device's stored presets
    /// and DPI target. Called on initial build, device switch, and preset
    /// commits. The cycle index resets to 0 since the list contents may
    /// have changed.
    fn sync_dpi_cycle(&self) {
        let presets = self
            .current_record()
            .map(|r| self.config.dpi_presets(&r.config_key))
            .unwrap_or_default();
        let target = self.current_record().and_then(|r| r.dpi_target.clone());
        match self.dpi_cycle.write() {
            Ok(mut guard) => {
                *guard = DpiCycleState {
                    presets,
                    index: 0,
                    target,
                };
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "dpi_cycle lock poisoned — hook will keep stale presets"
                );
            }
        }
    }
}

impl Global for AppState {}

fn build_device_list(inventories: &[DeviceInventory], cache: &AssetResolver) -> Vec<DeviceRecord> {
    let mut list = Vec::new();
    for inv in inventories {
        let receiver_uid = inv.receiver.unique_id.clone();
        for paired in &inv.paired {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            let config_key = model.config_key();
            let asset = cache.resolve(model);
            let display_name = asset
                .as_ref()
                .map(|a| a.display_name.clone())
                .or_else(|| paired.codename.clone())
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            let dpi_target = receiver_uid.as_ref().map(|uid| DpiTarget {
                receiver_uid: uid.clone(),
                slot: paired.slot,
            });
            list.push(DeviceRecord {
                config_key,
                display_name,
                asset,
                dpi_target,
            });
        }
    }
    list
}

fn pick_initial_device(list: &[DeviceRecord], saved: Option<&str>) -> usize {
    saved
        .and_then(|key| list.iter().position(|r| r.config_key == key))
        .unwrap_or(0)
}

fn bindings_for(
    config: &Config,
    record: Option<&DeviceRecord>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, Action> {
    let stored = record
        .map(|r| config.effective_bindings(&r.config_key, app_bundle))
        .unwrap_or_default();
    let mut bindings: BTreeMap<ButtonId, Action> = ButtonId::ALL
        .iter()
        .copied()
        .map(|b| (b, default_binding(b)))
        .collect();
    for (k, v) in stored {
        bindings.insert(k, v);
    }
    bindings
}

fn gesture_bindings_for(
    config: &Config,
    record: Option<&DeviceRecord>,
) -> BTreeMap<GestureDirection, Action> {
    let stored = record
        .map(|r| config.gesture_bindings_for(&r.config_key))
        .unwrap_or_default();
    let mut bindings: BTreeMap<GestureDirection, Action> = GestureDirection::ALL
        .iter()
        .copied()
        .map(|d| (d, default_gesture_binding(d)))
        .collect();
    for (k, v) in stored {
        bindings.insert(k, v);
    }
    bindings
}
