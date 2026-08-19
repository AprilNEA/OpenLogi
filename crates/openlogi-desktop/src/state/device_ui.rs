//! Consolidated per-device UI-state row.

use openlogi_core::config::LightSettings;

use super::SmartShiftWriteStatus;
use super::light::PendingLightCommand;

/// Everything `AppState` tracks per device outside the persisted config and
/// the swr-backed DPI, SmartShift, and onboard-profile reads.
///
/// Replaces the parallel `BTreeMap<String, _>` fields that all shared the
/// same device-key domain — manual camera-light override, volatile light
/// settings, an in-flight light command, the inventory-miss counter, a
/// pending SmartShift write id, and SmartShift write-confirmation status
/// — with one row per device. A device absent from the owning map is
/// equivalent to every field here at its default.
#[derive(Debug, Default)]
pub(super) struct DeviceUiState {
    /// Transient manual power choice for a camera-linked light, overriding
    /// the automatic camera-derived state until the next camera transition.
    pub(super) manual_light_override: Option<bool>,
    /// Session-only light settings for a device whose OS-node identity is
    /// not stable enough to persist to `config.toml`.
    pub(super) volatile_light: Option<LightSettings>,
    /// The in-flight optimistic standalone-light write, if any.
    pub(super) light_command: Option<PendingLightCommand>,
    /// Consecutive inventory snapshots that omitted this device.
    pub(super) inventory_misses: u8,
    /// Identity of the SmartShift write awaiting a confirming re-read.
    pub(super) smartshift_pending_confirm: Option<u64>,
    /// Visible outcome of the post-write SmartShift confirmation.
    pub(super) smartshift_write_status: Option<SmartShiftWriteStatus>,
}
