//! DPI presets and live writes. Capability discovery itself lives in
//! [`super::load::LazyDeviceData`], reached directly as `self.reads.dpi`.

use openlogi_core::hid::{DeviceRoute, Dpi, DpiCapabilities, DpiInfo, WriteError};
use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::device_key::DeviceKey;
use super::load::DpiStatus;
use super::{AppState, DEFAULT_DPI};

impl AppState {
    /// Replace the DPI preset list for the currently selected device. The
    /// new list is persisted to `config.toml` and pushed into the shared
    /// hook map so the next `CycleDpiPresets` press sees it. The cycle
    /// `index` is reset to 0 — the user just rebuilt the list, the old
    /// index is meaningless.
    ///
    /// No-op when no device is selected (binding panel won't expose the
    /// editor in that state).
    pub fn commit_dpi_presets(&mut self, presets: Vec<Dpi>) {
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
    pub fn dpi_presets(&self) -> Vec<Dpi> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(|key| self.config.dpi_presets(key))
            .unwrap_or_default()
    }
    /// The active device's known DPI, falling back to [`DEFAULT_DPI`] until its
    /// capability read completes. Used to seed `self.dpi` on a device switch.
    #[must_use]
    pub(crate) fn dpi_for_current(&self) -> Dpi {
        self.current_record()
            .and_then(|record| self.reads.dpi.get(&record.device_key()))
            .and_then(|status| match status {
                DpiStatus::Ready(info) => Some(info.current),
                _ => None,
            })
            .unwrap_or(DEFAULT_DPI)
    }
    /// Store a DPI capability discovery result if it still matches the known
    /// device route. This guards against async reads completing after the
    /// carousel or inventory changed.
    pub fn store_dpi_info(
        &mut self,
        key: DeviceKey,
        route: &DeviceRoute,
        result: Result<DpiInfo, WriteError>,
    ) {
        let is_active = self.current_record().is_some_and(|r| r.device_key() == key);
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.device_key() == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.device_key() == key);
        // Only the active device owns the shared `self.dpi`; a result landing for
        // a background device after a carousel switch must not clobber the
        // visible value.
        if let Some(info) = self.reads.dpi.store(
            key,
            result,
            dpi_error_is_permanent,
            matches_route,
            still_present,
            "DPI",
        ) && is_active
        {
            self.dpi = info.current;
        }
    }
    /// DPI capabilities for the active device, if discovery succeeded.
    #[must_use]
    pub fn active_dpi_capabilities(&self) -> Option<&DpiCapabilities> {
        self.current_record()
            .and_then(|record| self.reads.dpi.get(&record.device_key()))
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
    pub fn normalize_active_dpi(&self, dpi: Dpi) -> Dpi {
        self.active_dpi_capabilities()
            .map_or(dpi, |caps| caps.nearest(dpi))
    }
    /// Apply `dpi` to the active device (best-effort, via the agent) and
    /// persist it per device — the sensor value lives in device RAM and resets
    /// on a power cycle (#189), so the agent re-applies it on reconnect.
    /// Updates the displayed value even with no device selected.
    pub fn commit_dpi(&mut self, dpi: Dpi) {
        self.dpi = dpi;
        let Some(record) = self.current_record() else {
            debug!("no active device — DPI change kept in memory only");
            return;
        };
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        if let Some(persistent_key) = persistent_key {
            self.config.set_dpi(&persistent_key, dpi);
            if !self.persist_config("DPI") {
                return;
            }
        } else {
            debug!(
                key = record.config_key.as_str(),
                "transient device DPI applied without persistence"
            );
        }
        if let Some(route) = route {
            self.send_ipc(crate::services::ipc::Command::SetDpi(route, dpi));
        }
        // Apply-now is deliberately queued before the config reload. The IPC
        // client can coalesce rapid slider releases while this reload runs, so
        // the mouse no longer waits behind older config work before changing.
        self.send_ipc(crate::services::ipc::Command::ReloadConfig);
    }
}

pub(crate) fn dpi_error_is_permanent(error: &WriteError) -> bool {
    matches!(
        error,
        WriteError::FeatureUnsupported { .. } | WriteError::EmptyDpiList
    )
}
