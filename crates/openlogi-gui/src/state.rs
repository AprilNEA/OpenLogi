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
use std::sync::{Arc, RwLock};

use gpui::Global;
use openlogi_core::config::{AppSettings, Config};
use openlogi_core::device::DeviceInventory;
use openlogi_hook::Hook;
use tracing::{debug, warn};

mod bindings;
mod devices;
mod dpi;

pub use devices::DeviceRecord;
pub use dpi::DpiCycleState;

use crate::asset::AssetResolver;
use crate::data::mouse_buttons::{Action, ButtonId, GestureDirection};
use crate::state::bindings::{bindings_for, gesture_bindings_for};
use crate::state::devices::{build_device_list, pick_initial_device};

/// Default DPI value applied to a fresh AppState. Matches a common Logitech
/// mid-range mouse and keeps the dot-preview visually obvious from frame one.
pub const DEFAULT_DPI: u32 = 1600;

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
    /// vs "No device connected" empty state and is cleared once the inventory
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
    /// Shared gesture-direction binding map consumed by the gesture watcher
    /// thread. Mirrors [`Self::hook_bindings`] but keyed by direction; the
    /// watcher holds the other `Arc` clone, so writes here reach it without
    /// GPUI involvement.
    pub gesture_hook_bindings: Arc<RwLock<BTreeMap<GestureDirection, Action>>>,
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
        let gesture_arc = Arc::new(RwLock::new(BTreeMap::new()));
        let cycle_arc = Arc::new(RwLock::new(DpiCycleState::default()));
        Self::with_runtime_shared(
            config,
            inventories,
            cache,
            bindings_arc,
            gesture_arc,
            cycle_arc,
        )
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
        gesture_hook_bindings: Arc<RwLock<BTreeMap<GestureDirection, Action>>>,
        dpi_cycle: Arc<RwLock<DpiCycleState>>,
    ) -> Self {
        let device_list = build_device_list(inventories, cache);
        let current_device = pick_initial_device(&device_list, config.selected_device());
        let mut state = Self {
            current_device,
            current_app_bundle: None,
            active_button: None,
            accessibility_granted: Hook::has_accessibility(),
            scanning: true,
            button_bindings: BTreeMap::new(),
            gesture_bindings: BTreeMap::new(),
            dpi: DEFAULT_DPI,
            device_list,
            config,
            hook_bindings,
            dpi_cycle,
            gesture_hook_bindings,
        };
        state.button_bindings = state.bindings_for_current();
        state.gesture_bindings = state.gesture_bindings_for_current();
        state.sync_hook_bindings();
        state.sync_gesture_bindings();
        state.sync_dpi_cycle();
        state
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
        let bindings = bindings_for(config, record, None);
        let gesture_bindings = gesture_bindings_for(config, record);
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
        self.sync_gesture_bindings();
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
        self.sync_gesture_bindings();
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
        crate::platform::launch_agent::reconcile(enabled);
    }

    /// Toggle the macOS menu-bar (status item) icon, persist it, and apply it
    /// live. Turning it off hides the item *and* pins the app to Regular
    /// activation, so it stays an ordinary Dock app rather than being left with
    /// neither a window, a Dock icon, nor a menu-bar icon. No-op when unchanged.
    ///
    /// macOS-only: the toggle that calls it exists only there, so gating avoids
    /// an unused-method warning on other platforms.
    #[cfg(target_os = "macos")]
    pub fn set_show_in_menu_bar(&mut self, enabled: bool) {
        if self.config.app_settings.show_in_menu_bar == enabled {
            return;
        }
        self.config.app_settings.show_in_menu_bar = enabled;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist show-in-menu-bar setting");
        }
        #[cfg(target_os = "macos")]
        {
            crate::platform::tray::set_visible(enabled);
            if !enabled {
                crate::platform::tray::show_in_dock();
            }
        }
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

    /// Set the UI language (`None` = follow system), persist it, and switch the
    /// process-global locale live via [`crate::i18n`]. The caller must refresh
    /// open windows and rebuild the menu so everything re-renders. No-op when
    /// unchanged.
    pub fn set_language(&mut self, language: Option<String>) {
        if self.config.app_settings.language == language {
            return;
        }
        self.config.app_settings.language = language;
        if let Err(e) = self.config.save_atomic() {
            warn!(error = %e, "could not persist language setting");
        }
        crate::i18n::activate(self.config.app_settings.language.as_deref());
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

    /// Update a single gesture-button sub-binding in memory, on disk, and in the
    /// shared gesture map the watcher thread reads.
    pub fn commit_gesture_binding(&mut self, direction: GestureDirection, action: Action) {
        self.gesture_bindings.insert(direction, action.clone());

        match self.gesture_hook_bindings.write() {
            Ok(mut guard) => {
                guard.insert(direction, action.clone());
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "gesture_hook_bindings lock poisoned — change will not reach the watcher"
                );
            }
        }

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

    /// Mirror [`Self::gesture_bindings`] into the watcher-shared `Arc`. Called
    /// alongside [`Self::sync_hook_bindings`] after the gesture map changes
    /// wholesale (initial build, device switch).
    fn sync_gesture_bindings(&self) {
        match self.gesture_hook_bindings.write() {
            Ok(mut guard) => {
                *guard = self.gesture_bindings.clone();
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "gesture_hook_bindings lock poisoned — watcher will keep stale bindings"
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
        let target = self.current_record().and_then(|r| r.route.clone());
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
