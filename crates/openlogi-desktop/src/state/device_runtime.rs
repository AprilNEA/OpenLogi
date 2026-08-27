//! Consolidated per-device runtime-state row.

use super::light::LightDeviceState;
use super::smartshift::SmartShiftDeviceState;

/// Everything `AppState` tracks per device outside persisted configuration and
/// the swr-backed DPI/SmartShift reads.
///
/// Replaces six parallel `BTreeMap<String, _>` fields that all shared the
/// same device-key domain — manual camera-light override, volatile light
/// settings, an in-flight light command, the inventory-miss counter, a
/// SmartShift write lifecycle, and its confirmation status — with one row per
/// device. The row also keeps light-command status scoped to the device that
/// produced it. A device absent from the owning map is equivalent to every
/// field here at its default.
#[derive(Debug, Default)]
pub(super) struct DeviceRuntimeState {
    /// Consecutive inventory snapshots that omitted this device.
    pub(super) inventory_misses: u8,
    pub(super) smartshift: SmartShiftDeviceState,
    pub(super) light: LightDeviceState,
}
