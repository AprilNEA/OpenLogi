//! DPI presets and live writes. Capability discovery is an swr-backed query
//! owned by the device-read service.

use gpui::{App, Context};
use openlogi_core::hid::{Dpi, DpiCapabilities};
use tracing::debug;

use crate::state::devices::DeviceRecord;

use super::device_key::DeviceKey;
use super::load::DpiStatus;
use super::{AppState, DEFAULT_DPI, StateEvent};

impl AppState {
    pub(super) fn load_current_dpi(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.reads
            .ensure_dpi(key.clone(), route, self.ipc_sender(), cx);
        self.apply_dpi_read(&key);
    }

    pub(crate) fn retry_dpi_read(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            state.reads.retry_dpi(&key);
            cx.emit(StateEvent::DpiChanged(key));
        });
    }

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
            .and_then(|record| self.reads.dpi_load(&record.device_key()))
            .and_then(|status| match status {
                DpiStatus::Ready(info) => Some(info.current),
                _ => None,
            })
            .unwrap_or(DEFAULT_DPI)
    }
    /// Seed the active panel from the latest query. Query generations fence
    /// disconnected routes; this selected-device check prevents an old
    /// carousel page from changing the shared visible value.
    pub(crate) fn apply_dpi_read(&mut self, key: &DeviceKey) {
        if self
            .current_record()
            .is_none_or(|record| record.device_key() != *key)
        {
            return;
        }
        if let Some(DpiStatus::Ready(info)) = self.reads.dpi_load(key) {
            self.dpi = info.current;
        }
    }
    /// DPI capabilities for the active device, if discovery succeeded.
    #[must_use]
    pub fn active_dpi_capabilities(&self) -> Option<&DpiCapabilities> {
        self.current_record()
            .and_then(|record| self.reads.dpi_load(&record.device_key()))
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
            if !self.persist_and_reload("DPI") {
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
    }
}
