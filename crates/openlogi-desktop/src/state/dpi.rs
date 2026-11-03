//! DPI presets and live writes. Capability discovery is an swr-backed query
//! owned by the device-read service.
//!
//! A DPI write persists nothing until the device answers. The agent writes
//! the saved DPI again at every reconnect, so saving a value the device
//! refused or did not apply would show that value and re-send it at each
//! reconnect. Only a write the device took is saved. After any other answer
//! the editor shows the DPI the device reports, or the last one it reported.

use gpui::Context;
use openlogi_core::hid::{Dpi, DpiCapabilities, WriteError};
use tracing::{debug, warn};

use crate::state::devices::DeviceRecord;

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::DpiLoad;
use super::{AppState, DEFAULT_DPI, StateEvent};

impl AppState {
    pub(super) fn load_current_dpi(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_dpi(key.clone(), route, self.ipc_sender(), cx);
        self.apply_dpi_read(&key);
    }

    /// Re-run `key`'s exhausted DPI read — the "click to retry" affordance on
    /// a [`DpiLoad::Failed`] device.
    pub(crate) fn retry_dpi_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_dpi(key);
        StateEvent::DpiChanged(key.clone()).into()
    }

    /// Replace the DPI preset list for the currently selected device. The
    /// new list is persisted to `config.toml` and pushed into the shared
    /// hook map so the next `CycleDpiPresets` press sees it. The cycle
    /// `index` is reset to 0 — the user just rebuilt the list, the old
    /// index is meaningless.
    ///
    /// No-op when no device is selected (binding panel won't expose the
    /// editor in that state).
    pub fn commit_dpi_presets(&mut self, presets: Vec<Dpi>) -> StateEvents {
        let events = self.for_current_device(StateEvent::DpiChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!("no persistent device key — DPI presets kept in memory only");
            return events;
        };
        self.config
            .edit(|config| config.set_dpi_presets(&key, presets));
        self.persist_and_reload("DPI presets");
        events
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
    /// The active device's known DPI: the live capability read once it
    /// completes, else the persisted config value, else [`DEFAULT_DPI`] for a
    /// device with neither yet. Used to seed the pointer editor on a device
    /// switch — without the config fallback, a device with a real configured
    /// DPI briefly (or, if the read never completes, indefinitely) showed the
    /// unrelated hardcoded default instead of the value the user actually set.
    #[must_use]
    pub(crate) fn dpi_for_current(&self) -> Dpi {
        let Some(record) = self.current_record() else {
            return DEFAULT_DPI;
        };
        let live = self.pointer.reads.dpi_load(&record.device_key());
        let configured = record
            .persistent_config_key()
            .and_then(|key| self.config.devices.get(key))
            .and_then(|device| device.effective_dpi(&record.route_key));
        resolve_dpi(live, configured)
    }
    /// Seed the active panel from the latest query. Query flights fence
    /// disconnected routes; this selected-device check prevents an old
    /// gallery card from changing the shared visible value.
    pub(crate) fn apply_dpi_read(&mut self, key: &DeviceKey) {
        if !self.is_current_device(key) {
            return;
        }
        if let Some(DpiLoad::Ready(info)) = self.pointer.reads.dpi_load(key) {
            self.pointer.dpi = info.current;
        }
    }
    /// DPI capabilities for the active device, if discovery succeeded.
    #[must_use]
    pub fn active_dpi_capabilities(&self) -> Option<&DpiCapabilities> {
        self.current_record()
            .and_then(|record| self.pointer.reads.dpi_load(&record.device_key()))
            .and_then(|status| match status {
                DpiLoad::Ready(info) => Some(&info.capabilities),
                DpiLoad::Unknown
                | DpiLoad::Loading
                | DpiLoad::Failed(_)
                | DpiLoad::Unsupported(_) => None,
            })
    }
    /// Snap `dpi` to the active device's supported list when known.
    #[must_use]
    pub fn normalize_active_dpi(&self, dpi: Dpi) -> Dpi {
        self.active_dpi_capabilities()
            .map_or(dpi, |caps| caps.nearest(dpi))
    }
    /// Show `dpi` for the active device and write it through the agent. The
    /// device's answer arrives as [`Self::apply_dpi_written`], which persists
    /// `dpi` only if the device took it. With no device selected, or none
    /// reachable to write, the value is shown and kept in memory only.
    pub fn commit_dpi(&mut self, dpi: Dpi) -> StateEvents {
        let events = self.for_current_device(StateEvent::DpiChanged);
        self.pointer.dpi = dpi;
        let Some(record) = self.current_record() else {
            debug!("no active device — DPI change kept in memory only");
            return events;
        };
        let key = record.device_key();
        let Some(route) = record.route.clone() else {
            debug!(key = %key, "device unreachable — DPI change kept in memory only");
            return events;
        };
        let write = self.pointer.next_dpi_write;
        self.pointer.next_dpi_write = write.saturating_add(1);
        self.pointer.pending_dpi_writes.insert(key.clone(), write);
        self.send_ipc(crate::services::ipc::SetDpi {
            route,
            dpi,
            key,
            write,
        });
        events
    }

    /// The device's answer to a [`Self::commit_dpi`] write of `dpi`.
    ///
    /// `Ok` persists `dpi` per device, because the sensor value lives in
    /// device RAM and resets on a power cycle (#189): the agent writes the
    /// saved value again on reconnect. A failure saves nothing.
    /// [`WriteError::DpiNotApplied`] shows the DPI the device reports instead.
    /// [`WriteError::DpiUnsupported`] wrote nothing, so the device's last
    /// known DPI is shown again. Any other failure also shows that value and
    /// reads the device again, because the write may or may not have reached
    /// it.
    ///
    /// The agent answers writes in the order they were sent. An older write's
    /// answer still records what the device did, but only the newest write's
    /// answer changes the shown DPI.
    ///
    /// The agent also answers `Ok` for a write that a newer one at the mouse,
    /// such as a precision-mode press, superseded before its turn. That write
    /// is saved too, because the answer cannot tell the two apart.
    pub fn apply_dpi_written(
        &mut self,
        key: &DeviceKey,
        write: u64,
        dpi: Dpi,
        result: Result<(), WriteError>,
    ) -> StateEvents {
        let newest = self.pointer.pending_dpi_writes.get(key) == Some(&write);
        if newest {
            self.pointer.pending_dpi_writes.remove(key);
        }
        // `None` shows the DPI the device is known to run at.
        let reported = match result {
            Ok(()) => {
                self.pointer.reads.set_dpi_current(key, dpi);
                self.persist_dpi(key, dpi);
                Some(dpi)
            }
            Err(WriteError::DpiNotApplied { actual, .. }) => {
                warn!(key = %key, requested = %dpi, %actual, "device kept another DPI — not saving the request");
                self.pointer.reads.set_dpi_current(key, actual);
                Some(actual)
            }
            Err(error @ WriteError::DpiUnsupported { .. }) => {
                warn!(%error, key = %key, "device refused the DPI — not saving it");
                None
            }
            Err(error) => {
                warn!(%error, key = %key, requested = %dpi, "DPI write did not land — not saving it, re-reading the device");
                self.pointer.reads.refresh_dpi(key);
                None
            }
        };
        if newest && self.is_current_device(key) {
            self.pointer.dpi = reported.unwrap_or_else(|| self.dpi_for_current());
        }
        StateEvent::DpiChanged(key.clone()).into()
    }

    /// Save `dpi` for `key`'s device after the device took it. A device with
    /// a transient identity, or one forgotten while the write was in flight,
    /// has nothing to save under.
    fn persist_dpi(&mut self, key: &DeviceKey, dpi: Dpi) {
        let Some(persistent_key) = self
            .devices
            .records
            .iter()
            .find(|record| record.config_key == key.as_str())
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            debug!(key = %key, "transient device DPI applied without persistence");
            return;
        };
        self.config
            .edit(|config| config.set_dpi(&persistent_key, dpi));
        self.persist_and_reload("DPI");
    }

    /// The DPI value currently shown by the active pointer editor.
    #[must_use]
    pub fn dpi(&self) -> Dpi {
        self.pointer.dpi
    }

    /// Update the pointer editor's in-progress DPI value without committing it.
    pub fn set_dpi_preview(&mut self, dpi: Dpi) -> StateEvents {
        self.pointer.dpi = dpi;
        self.for_current_device(StateEvent::DpiChanged)
    }

    /// What is known of `key`'s DPI; [`DpiLoad::Unknown`] for a device nobody
    /// has queried.
    pub(crate) fn dpi_load_for(&self, key: &DeviceKey) -> DpiLoad {
        self.pointer
            .reads
            .dpi_load(key)
            .cloned()
            .unwrap_or_default()
    }
}

/// [`AppState::dpi_for_current`]'s decision, as a pure function of the two
/// facts it resolves between: the live capability read and the persisted
/// config value. The live read always wins once it lands — the point of
/// reading it at all is to show the sensor's real value, not the user's
/// request, which a firmware that clamps or rejects it may not have honored.
#[must_use]
pub(super) fn resolve_dpi(live: Option<&DpiLoad>, configured: Option<Dpi>) -> Dpi {
    let from_live = live.and_then(|status| match status {
        DpiLoad::Ready(info) => Some(info.current),
        _ => None,
    });
    from_live.or(configured).unwrap_or(DEFAULT_DPI)
}
