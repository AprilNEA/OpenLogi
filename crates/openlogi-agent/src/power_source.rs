//! Publishes receiver accessories to the experimental macOS Batteries integration.
//!
//! Device eligibility and offline lifetime have one owner here. The backend owns
//! native resources; tests drive the same policy without loading IOKit.

mod iokit;

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

use openlogi_core::config::Config;
use openlogi_core::device::{BatteryStatus, BatteryWidgetStatus, DeviceInventory, DeviceKind};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::{DeviceRoute, ReceiverBrand, find_receiver};
use tracing::warn;

pub use iokit::IoKitPowerSourceBackend;

const OFFLINE_GRACE: Duration = Duration::from_mins(5);

/// One accessory reading accepted for publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessoryPower {
    /// Stable receiver-and-slot identity.
    pub identifier: String,
    /// User's custom name, or the device's model name.
    pub name: String,
    /// Category interpreted by macOS.
    pub category: AccessoryCategory,
    /// Percentage reported or estimated by OpenLogi, bounded to 0..=100.
    pub percentage: u8,
    /// Reported charging state, including fully charged external power.
    pub status: BatteryStatus,
}

/// Supported macOS accessory categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessoryCategory {
    /// A mouse behind a receiver.
    Mouse,
    /// A keyboard behind a receiver.
    Keyboard,
}

impl AccessoryCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mouse => "Mouse",
            Self::Keyboard => "Keyboard",
        }
    }
}

/// Failure isolated to the optional battery publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerSourceError {
    /// This macOS installation lacks the private API.
    Unavailable(&'static str),
    /// A native operation returned an IOKit error code.
    Operation {
        /// Name of the failed native operation.
        operation: &'static str,
        /// Native return code, preserved for diagnostics.
        code: i32,
    },
    /// The native create operation succeeded without returning a handle.
    MissingHandle,
}

impl fmt::Display for PowerSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => formatter.write_str(reason),
            Self::Operation { operation, code } => {
                write!(
                    formatter,
                    "{operation} failed (0x{:08x})",
                    code.cast_unsigned()
                )
            }
            Self::MissingHandle => formatter.write_str("IOPSCreatePowerSource returned no handle"),
        }
    }
}

impl std::error::Error for PowerSourceError {}

/// Native operations used by the publisher and its hardware-free test backend.
pub trait PowerSourceBackend {
    /// Resolve optional symbols. Called only while the setting is enabled.
    fn prepare(&mut self) -> Result<(), PowerSourceError>;
    /// Create or update an entry; success means macOS accepted the details.
    fn upsert(&mut self, accessory: &AccessoryPower) -> Result<(), PowerSourceError>;
    /// Consume a handle, including on error. Apple's release frees its argument,
    /// so callers must never retry removal with the same handle.
    fn remove(&mut self, identifier: &str) -> Result<(), PowerSourceError>;
}

#[derive(Default)]
struct Tracked {
    offline_since: Option<Instant>,
    /// Only successful publications count as active or suppress later writes.
    published: Option<AccessoryPower>,
}

/// Owns publication policy, error status and the offline grace period.
pub struct PowerSourcePublisher<B: PowerSourceBackend> {
    backend: B,
    tracked: BTreeMap<String, Tracked>,
    status: BatteryWidgetStatus,
}

impl<B: PowerSourceBackend> PowerSourcePublisher<B> {
    /// Construct a disabled publisher without consulting the native backend.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            tracked: BTreeMap::new(),
            status: BatteryWidgetStatus::Disabled,
        }
    }

    /// Current status, based on successful native operations.
    pub fn status(&self) -> BatteryWidgetStatus {
        self.status.clone()
    }

    /// Earliest pending offline removal, for the owning loop's timer.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.tracked
            .values()
            .filter_map(|tracked| tracked.offline_since.map(|since| since + OFFLINE_GRACE))
            .min()
    }

    /// Release all native entries immediately, also used at shutdown.
    pub fn clear_all(&mut self) {
        let mut error = None;
        for (identifier, _) in std::mem::take(&mut self.tracked) {
            if let Err(failure) = self.backend.remove(&identifier) {
                error.get_or_insert(failure);
            }
        }
        self.update_status(error.map_or(BatteryWidgetStatus::Disabled, |error| {
            BatteryWidgetStatus::Failed {
                published_devices: 0,
                reason: error.to_string(),
            }
        }));
    }

    /// Apply a snapshot and configuration at a caller-supplied monotonic time.
    /// Retried failures are bounded by the owning loop's reconciliation cadence.
    pub fn reconcile(&mut self, config: &Config, inventories: &[DeviceInventory], now: Instant) {
        if !config.app_settings.macos_battery_widget {
            self.clear_all();
            return;
        }
        if let Err(error) = self.backend.prepare() {
            self.update_status(BatteryWidgetStatus::Unavailable {
                reason: error.to_string(),
            });
            return;
        }

        let online = online_accessories(config, inventories, &self.tracked);
        let mut error = None;
        for (identifier, accessory) in &online {
            // Presence is independent of telemetry and cancels any offline deadline.
            if let Some(tracked) = self.tracked.get_mut(identifier) {
                tracked.offline_since = None;
            }
            let Some(accessory) = accessory else {
                continue;
            };
            let tracked = self.tracked.entry(identifier.clone()).or_default();
            if tracked.published.as_ref() != Some(accessory) {
                match self.backend.upsert(accessory) {
                    Ok(()) => tracked.published = Some(accessory.clone()),
                    Err(failure) => {
                        error.get_or_insert(failure);
                    }
                }
            }
        }

        self.tracked.retain(|identifier, tracked| {
            if online.contains_key(identifier) {
                return true;
            }
            let since = *tracked.offline_since.get_or_insert(now);
            if now.saturating_duration_since(since) < OFFLINE_GRACE {
                return true;
            }
            if let Err(failure) = self.backend.remove(identifier) {
                error.get_or_insert(failure);
            }
            false
        });

        let published_devices = self
            .tracked
            .values()
            .filter(|tracked| tracked.published.is_some())
            .count();
        self.update_status(error.map_or(
            BatteryWidgetStatus::Active { published_devices },
            |error| BatteryWidgetStatus::Failed {
                published_devices,
                reason: error.to_string(),
            },
        ));
    }

    fn update_status(&mut self, status: BatteryWidgetStatus) {
        if self.status != status {
            match &status {
                BatteryWidgetStatus::Unavailable { reason }
                | BatteryWidgetStatus::Failed { reason, .. } => {
                    warn!(%reason, "macOS battery widget integration failed");
                }
                _ => {}
            }
            self.status = status;
        }
    }
}

impl<B: PowerSourceBackend> Drop for PowerSourcePublisher<B> {
    fn drop(&mut self) {
        self.clear_all();
    }
}

/// Online presence is recorded even when no battery reading is available.
fn online_accessories(
    config: &Config,
    inventories: &[DeviceInventory],
    tracked: &BTreeMap<String, Tracked>,
) -> BTreeMap<String, Option<AccessoryPower>> {
    let mut online = BTreeMap::new();
    for inventory in inventories {
        let Some(receiver) =
            find_receiver(inventory.receiver.vendor_id, inventory.receiver.product_id)
        else {
            continue;
        };
        if !matches!(
            receiver.brand,
            ReceiverBrand::Bolt | ReceiverBrand::Unifying
        ) {
            continue;
        }
        for paired in &inventory.paired {
            if !paired.online {
                continue;
            }
            let category = match paired.kind {
                DeviceKind::Mouse => AccessoryCategory::Mouse,
                DeviceKind::Keyboard => AccessoryCategory::Keyboard,
                _ => continue,
            };
            let Some(route @ (DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. })) =
                DeviceRoute::for_slot(inventory, paired.slot)
            else {
                continue;
            };
            let stable_id = DeviceStableId::from_parts(Some(&route), paired.slot, None, [0; 4]);
            let identifier = stable_id.runtime_key();
            let identity = paired.model_info.as_ref().map(|model| {
                DeviceIdentity::from_parts(model.serial_number.as_deref(), model.unit_id)
            });
            let config_key = config.resolve_device_key(&stable_id, identity.as_ref());
            // Metadata can change while telemetry is missing. Reuse only an
            // accepted reading, never invent a battery level for a new device.
            let battery = paired
                .battery
                .as_ref()
                .map(|battery| (battery.percentage.min(100), battery.status))
                .or_else(|| {
                    tracked
                        .get(&identifier)
                        .and_then(|tracked| tracked.published.as_ref())
                        .map(|published| (published.percentage, published.status))
                });
            let reading = battery.map(|(percentage, status)| AccessoryPower {
                name: config_key
                    .as_ref()
                    .and_then(|key| config.device_custom_name(key.as_str()))
                    .or(paired.codename.as_deref())
                    .unwrap_or("Logitech Device")
                    .to_owned(),
                identifier: identifier.clone(),
                category,
                percentage,
                status,
            });
            online.insert(identifier, reading);
        }
    }
    online
}

#[cfg(test)]
mod tests;
