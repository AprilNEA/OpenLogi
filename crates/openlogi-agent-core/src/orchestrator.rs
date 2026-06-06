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
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, RwLock};

use openlogi_core::config::Config;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{CaptureChannel, DIRECT_DEVICE_INDEX, DeviceRoute};
use tracing::warn;

use crate::DpiCycleState;
use crate::bindings::{bindings_for, gesture_bindings_for};
use crate::hook_runtime::BindingMap;
use crate::watchers::gesture::GestureBindings;

/// The minimal per-device facts the agent needs: the config key (binding /
/// preset lookup) and the HID++ route (DPI/SmartShift writes + capture target).
struct AgentDevice {
    config_key: String,
    route: Option<DeviceRoute>,
}

/// The shared runtime handed to the hook and the gesture watcher. Every field
/// is an `Arc`, so cloning is cheap; the orchestrator rewrites the inner values
/// on each rebuild and the background threads observe them on their next read.
#[derive(Clone)]
pub struct SharedRuntime {
    pub hook_bindings: BindingMap,
    pub gesture_bindings: GestureBindings,
    pub dpi_cycle: Arc<RwLock<DpiCycleState>>,
    pub thumbwheel_sensitivity: Arc<AtomicI32>,
    pub capture_channel: CaptureChannel,
}

/// Owns the config + device selection and keeps [`SharedRuntime`] in sync.
pub struct Orchestrator {
    config: Config,
    devices: Vec<AgentDevice>,
    current: usize,
    current_app: Option<String>,
    shared: SharedRuntime,
}

impl Orchestrator {
    /// Build from a loaded config. Creates the shared `Arc`s and seeds them
    /// from the config with no devices yet; the first inventory tick fills in
    /// the routes and presets.
    #[must_use]
    pub fn new(config: Config) -> Self {
        let shared = SharedRuntime {
            hook_bindings: Arc::new(RwLock::new(BTreeMap::new())),
            gesture_bindings: Arc::new(RwLock::new(BTreeMap::new())),
            dpi_cycle: Arc::new(RwLock::new(DpiCycleState::default())),
            thumbwheel_sensitivity: Arc::new(AtomicI32::new(
                config.app_settings.thumbwheel_sensitivity,
            )),
            capture_channel: Arc::new(RwLock::new(None)),
        };
        let orch = Self {
            config,
            devices: Vec::new(),
            current: 0,
            current_app: None,
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
        write_value(
            &self.shared.hook_bindings,
            bindings_for(&self.config, key, self.current_app.as_deref()),
            "hook_bindings",
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

    /// Apply a fresh inventory snapshot: rebuild the device list, re-pick the
    /// selected device (by saved `config_key`, else the first), and rebuild.
    pub fn refresh_inventory(&mut self, inventories: &[DeviceInventory]) {
        self.devices = build_devices(inventories);
        self.current = pick_current(&self.devices, self.config.selected_device());
        self.rebuild();
    }

    /// Foreground-app change → re-overlay per-app bindings (hook map only;
    /// gestures and DPI are not app-scoped).
    pub fn set_current_app(&mut self, bundle: Option<String>) {
        if bundle == self.current_app {
            return;
        }
        self.current_app = bundle;
        write_value(
            &self.shared.hook_bindings,
            bindings_for(
                &self.config,
                self.current_key(),
                self.current_app.as_deref(),
            ),
            "hook_bindings",
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
            });
        }
    }
    devices
}

/// Index of the selected device: the one whose `config_key` matches the saved
/// selection, else the first. (The GUI orders its list for a stable carousel;
/// the agent only needs the selected device, so it keeps enumeration order —
/// the order matters solely for this no-selection fallback.)
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
