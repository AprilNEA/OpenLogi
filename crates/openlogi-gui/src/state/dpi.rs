//! DPI load state, presets, and live writes.

use openlogi_hid::{DeviceRoute, DpiCapabilities, DpiInfo, WriteError};
use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::load::DpiStatus;
use super::{AppState, DEFAULT_DPI};

impl AppState {
    /// The cached DPI-discovery status for `key`, for the diagnostics report.
    #[must_use]
    pub fn dpi_status_for(&self, key: &str) -> Option<DpiStatus> {
        self.dpi_data.get(key).cloned()
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
    pub(crate) fn dpi_for_current(&self) -> u32 {
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
}

pub(crate) fn dpi_error_is_permanent(error: &WriteError) -> bool {
    matches!(
        error,
        WriteError::FeatureUnsupported { .. } | WriteError::EmptyDpiList
    )
}
