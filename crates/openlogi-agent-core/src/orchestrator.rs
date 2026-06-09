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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, RwLock};

use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{CaptureChannel, DIRECT_DEVICE_INDEX, DeviceRoute};
use tracing::warn;

use crate::DpiCycleState;
use crate::bindings::{bindings_for, gesture_bindings_for, oshook_gestures_for};
use crate::device_order::DeviceStableId;
use crate::hook_runtime::{HookMaps, SharedHookMaps};
use crate::watchers::gesture::GestureBindings;

/// The minimal per-device facts the agent needs: the config key (binding /
/// preset lookup), the HID++ route (DPI/SmartShift writes + capture target), and
/// the identity fields the canonical ordering keys on (so the no-selection
/// fallback agrees with the GUI carousel — see [`crate::device_order`]).
struct AgentDevice {
    config_key: String,
    route: Option<DeviceRoute>,
    slot: u8,
    serial: Option<String>,
    unit_id: [u8; 4],
}

/// The shared runtime handed to the hook and the gesture watcher. Every field
/// is an `Arc`, so cloning is cheap; the orchestrator rewrites the inner values
/// on each rebuild and the background threads observe them on their next read.
#[derive(Clone)]
pub struct SharedRuntime {
    /// The OS-hook callback's single-action + gesture maps, behind one lock so a
    /// rebuild publishes both atomically (see [`HookMaps`]). Also read by the
    /// gesture watcher for the thumb-wheel/DPI-button single actions.
    pub hook_maps: SharedHookMaps,
    pub gesture_bindings: GestureBindings,
    pub dpi_cycle: Arc<RwLock<DpiCycleState>>,
    pub thumbwheel_sensitivity: Arc<AtomicI32>,
    pub capture_channel: CaptureChannel,
    /// Set while a pairing session runs: the gesture watcher then releases its
    /// capture session so `run_pairing` can own the receiver's HID node (one
    /// process still can't read the same node through two channels).
    pub pairing_active: Arc<AtomicBool>,
    /// Published by the gesture watcher: `true` when it holds no capture
    /// session, so the pairing manager can wait for capture to actually release
    /// before opening the receiver.
    pub capture_idle: Arc<AtomicBool>,
}

/// Owns the config + device selection and keeps [`SharedRuntime`] in sync.
pub struct Orchestrator {
    config: Config,
    devices: Vec<AgentDevice>,
    current: usize,
    current_app: Option<String>,
    /// The latest inventory snapshot, kept so the IPC server can answer the
    /// GUI's `inventory()` polls without re-enumerating (the agent owns all
    /// device I/O).
    last_inventory: Vec<DeviceInventory>,
    shared: SharedRuntime,
}

impl Orchestrator {
    /// Build from a loaded config. Creates the shared `Arc`s and seeds them
    /// from the config with no devices yet; the first inventory tick fills in
    /// the routes and presets.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let shared = SharedRuntime {
            hook_maps: Arc::new(RwLock::new(HookMaps::default())),
            gesture_bindings: Arc::new(RwLock::new(BTreeMap::new())),
            dpi_cycle: Arc::new(RwLock::new(DpiCycleState::default())),
            thumbwheel_sensitivity: Arc::new(AtomicI32::new(
                config.app_settings.thumbwheel_sensitivity,
            )),
            capture_channel: Arc::new(RwLock::new(None)),
            pairing_active: Arc::new(AtomicBool::new(false)),
            capture_idle: Arc::new(AtomicBool::new(true)),
        };
        let orch = Self {
            config,
            devices: Vec::new(),
            current: 0,
            current_app: None,
            last_inventory: Vec::new(),
            shared,
        };
        orch.rebuild();
        orch
    }

    /// A cheap clone of the shared `Arc`s to hand to the watchers and hook.
    #[must_use]
    pub fn shared(&self) -> SharedRuntime {
        self.shared.clone()
    }

    fn current_key(&self) -> Option<&str> {
        self.devices
            .get(self.current)
            .map(|d| d.config_key.as_str())
    }

    fn current_route(&self) -> Option<DeviceRoute> {
        self.devices.get(self.current).and_then(|d| d.route.clone())
    }

    /// Rewrite every shared map from the current config + selected device.
    fn rebuild(&self) {
        let key = self.current_key();
        // One write publishes both hook maps atomically, so a button press during
        // an owner switch can't observe a half-updated state.
        let app = self.current_app.as_deref();
        write_value(
            &self.shared.hook_maps,
            HookMaps {
                bindings: bindings_for(&self.config, key, app),
                gestures: oshook_gestures_for(&self.config, key, app),
            },
            "hook_maps",
        );
        write_value(
            &self.shared.gesture_bindings,
            gesture_bindings_for(&self.config, key),
            "gesture_bindings",
        );
        write_value(
            &self.shared.dpi_cycle,
            DpiCycleState {
                presets: key.map(|k| self.config.dpi_presets(k)).unwrap_or_default(),
                index: 0,
                target: self.current_route(),
                capabilities: None,
            },
            "dpi_cycle",
        );
        self.shared.thumbwheel_sensitivity.store(
            self.config.app_settings.thumbwheel_sensitivity,
            Ordering::Relaxed,
        );
    }

    /// Apply a fresh inventory snapshot. Always refreshes the snapshot the IPC
    /// `inventory()` poll serves (battery / online state changes without
    /// altering the device *set*), but only re-picks the selection and rebuilds
    /// the shared maps when the device set actually changed — `rebuild()` is
    /// driven solely by `config_key` + route and resets the live DPI-cycle
    /// index, so running it every 2s tick on an unchanged set would snap DPI
    /// back to `preset[0]` (and burn three `RwLock` writes) for nothing.
    pub fn refresh_inventory(&mut self, inventories: &[DeviceInventory]) {
        self.last_inventory = inventories.to_vec();
        let devices = build_devices(inventories);
        let changed = devices.len() != self.devices.len()
            || devices
                .iter()
                .zip(&self.devices)
                .any(|(a, b)| a.config_key != b.config_key || a.route != b.route);
        if !changed {
            return;
        }
        self.devices = devices;
        self.current = pick_current(&self.devices, self.config.selected_device());
        self.rebuild();
    }

    /// The latest inventory snapshot (for the IPC `inventory()` poll).
    #[must_use]
    pub fn inventory(&self) -> Vec<DeviceInventory> {
        self.last_inventory.clone()
    }

    /// Whether autostart is enabled in the current config (for IPC `status`).
    #[must_use]
    pub fn launch_at_login(&self) -> bool {
        self.config.app_settings.launch_at_login
    }

    /// Foreground-app change → re-overlay per-app bindings on the hook maps (DPI
    /// and the thumb-pad gesture map are not app-scoped, so they're untouched).
    /// Both hook maps are recomputed: a per-app override of the gesture owner
    /// turns it into a single action for that app, dropping it from the OS-hook
    /// gesture set — so the gesture map is app-scoped too.
    pub fn set_current_app(&mut self, bundle: Option<String>) {
        if bundle == self.current_app {
            return;
        }
        self.current_app = bundle;
        let key = self.current_key();
        let app = self.current_app.as_deref();
        write_value(
            &self.shared.hook_maps,
            HookMaps {
                bindings: bindings_for(&self.config, key, app),
                gestures: oshook_gestures_for(&self.config, key, app),
            },
            "hook_maps",
        );
    }

    /// Replace the config (after `config.toml` changed) and rebuild everything.
    pub fn reload_config(&mut self, config: Config) {
        self.config = config;
        self.current = pick_current(&self.devices, self.config.selected_device());
        self.rebuild();
    }
}

/// Build the agent device list from an inventory snapshot. Mirrors the GUI's
/// `build_device_list` minus the asset/display fields: a device is included
/// only once its HID++ DeviceInformation (`model_info`) has resolved, since the
/// `config_key` is derived from it.
fn build_devices(inventories: &[DeviceInventory]) -> Vec<AgentDevice> {
    let mut devices = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            devices.push(AgentDevice {
                config_key: model.config_key(),
                route: device_route(inv, paired.slot),
                slot: paired.slot,
                serial: model.serial_number.clone(),
                unit_id: model.unit_id,
            });
        }
    }
    // Order by the same canonical key the GUI carousel uses, so the
    // no-saved-selection fallback (`pick_current` -> index 0) targets the device
    // the GUI shows first rather than whatever HID node enumerated first.
    // `config_key` only breaks ties a unique `DeviceStableId` never produces.
    devices.sort_by(|a, b| {
        DeviceStableId::from_parts(a.route.as_ref(), a.slot, a.serial.as_deref(), a.unit_id)
            .cmp(&DeviceStableId::from_parts(
                b.route.as_ref(),
                b.slot,
                b.serial.as_deref(),
                b.unit_id,
            ))
            .then_with(|| a.config_key.cmp(&b.config_key))
    });
    devices
}

/// Index of the selected device: the one whose `config_key` matches the saved
/// selection, else the first. `build_devices` sorts by the same canonical key
/// the GUI carousel uses, so "the first" is the same physical device in both
/// processes even when nothing is persisted yet.
fn pick_current(devices: &[AgentDevice], saved: Option<&str>) -> usize {
    saved
        .and_then(|key| devices.iter().position(|d| d.config_key == key))
        .unwrap_or(0)
}

/// Build the [`DeviceRoute`] HID++ writes use to reach a device. A Bolt-paired
/// device routes through its receiver UID + slot; a directly attached one
/// (USB / Bluetooth) carries no receiver UID and sits at [`DIRECT_DEVICE_INDEX`],
/// routing by vendor/product id. A Bolt device whose receiver UID is unknown
/// gets no route, so writes are skipped rather than mis-routed.
fn device_route(inv: &DeviceInventory, slot: u8) -> Option<DeviceRoute> {
    match &inv.receiver.unique_id {
        Some(receiver_uid) => Some(DeviceRoute::Bolt {
            receiver_uid: receiver_uid.clone(),
            slot,
        }),
        None if slot == DIRECT_DEVICE_INDEX => Some(DeviceRoute::Direct {
            vendor_id: inv.receiver.vendor_id,
            product_id: inv.receiver.product_id,
        }),
        None => None,
    }
}

/// Replace the value behind an `RwLock`, logging (not panicking) on poison so a
/// background thread that paniced while holding the lock can't take the agent
/// down — it just keeps the stale value until the next successful rebuild.
fn write_value<T>(lock: &RwLock<T>, value: T, name: &str) {
    match lock.write() {
        Ok(mut guard) => *guard = value,
        Err(e) => warn!(error = %e, lock = name, "lock poisoned — keeping stale value"),
    }
}
