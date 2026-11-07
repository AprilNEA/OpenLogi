//! Changing a device's saved settings from inside the agent, with the effect
//! a change in the settings app has: the DPI, the SmartShift wheel, and
//! whether OpenLogi manages the device.
//!
//! An online device takes a DPI or wheel change first, so a value it refuses
//! is never saved and re-sent at every reconnect. `config.toml` then saves
//! the change through [`DeviceEdit`] — only while the file is still at the
//! revision read before the write — and the agent reloads the saved file
//! through its one reload path, [`ConfigReload`]. An offline device's DPI
//! and wheel are only saved: the agent writes them when it reconnects.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use openlogi_core::config::{ConfigError, DeviceEdit, DeviceEditError, DeviceSetting};
use openlogi_core::device_order::DeviceIdentity;
use openlogi_hid::{DeviceRoute, Dpi, SmartShiftStatus, WriteError};
use openlogi_ipc::ConfigReloadError;
use thiserror::Error;
use tokio::sync::Mutex;

use super::devices::stable_id;
use super::{Orchestrator, SharedHandles};

/// The agent's reload of `config.toml`, which every saved edit is followed
/// by. The agent binary owns it, since a reload also restyles its menu-bar
/// item and reconciles its launch at login.
pub trait ConfigReload: Send + Sync {
    /// Load `config.toml` into the running agent.
    fn reload_config_file(&self) -> impl Future<Output = Result<(), ConfigReloadError>> + Send;
}

/// Why a setting was not changed and saved.
#[derive(Debug, Error)]
pub enum SettingError {
    /// The agent knows no device at the route.
    #[error("no device is known at {0}")]
    UnknownDevice(DeviceRoute),
    /// The configuration cannot take the setting, or could not be read.
    /// Nothing was written.
    #[error(transparent)]
    Refused(#[from] DeviceEditError),
    /// The device did not take the setting. Nothing was saved.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// `config.toml` was not saved: another writer changed it since it was
    /// read ([`ConfigError::Conflict`]), or writing it failed. An online
    /// device keeps the setting it took.
    #[error("config.toml was not saved: {0}")]
    NotSaved(ConfigError),
    /// `config.toml` was saved, but the agent did not reload it.
    #[error("config.toml was saved, but the agent did not reload it: {}", .0.message)]
    NotReloaded(ConfigReloadError),
}

/// Changes, saves and reloads the settings of the devices the agent knows.
/// Cheap to clone.
#[derive(Clone)]
pub struct DeviceSettings<R> {
    orchestrator: Arc<Mutex<Orchestrator>>,
    shared: SharedHandles,
    reload: R,
    /// The config file saved; `None` saves the user's.
    path: Option<PathBuf>,
}

impl<R: ConfigReload> DeviceSettings<R> {
    /// Change the settings of the devices `orchestrator` knows, writing them
    /// through `shared` and reloading the user's config through `reload`.
    #[must_use]
    pub fn new(orchestrator: Arc<Mutex<Orchestrator>>, shared: SharedHandles, reload: R) -> Self {
        Self {
            orchestrator,
            shared,
            reload,
            path: None,
        }
    }

    /// [`Self::new`], saving the config at `path`.
    #[cfg(test)]
    pub(super) fn saving_to(self, path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ..self
        }
    }

    /// Set the DPI of the mouse at `route` — one of its `dpi_presets`, or any
    /// other value it supports — and save it.
    pub async fn set_dpi(&self, route: &DeviceRoute, dpi: Dpi) -> Result<(), SettingError> {
        let (edit, online) = self.stage(route, DeviceSetting::Dpi(dpi)).await?;
        if online {
            self.shared.set_dpi(route, dpi).await?;
        }
        self.save(edit).await
    }

    /// Set the SmartShift wheel of the mouse at `route` and save it.
    pub async fn set_smartshift(
        &self,
        route: &DeviceRoute,
        status: SmartShiftStatus,
    ) -> Result<(), SettingError> {
        let (edit, online) = self
            .stage(route, DeviceSetting::SmartShift(status.into()))
            .await?;
        if online {
            self.shared.set_smartshift(route, status).await?;
        }
        self.save(edit).await
    }

    /// Set whether OpenLogi manages the device at `route` and save it. The
    /// reload puts it into effect: a device no longer managed is left native.
    pub async fn set_enabled(
        &self,
        route: &DeviceRoute,
        enabled: bool,
    ) -> Result<(), SettingError> {
        let (edit, _) = self.stage(route, DeviceSetting::Enabled(enabled)).await?;
        self.save(edit).await
    }

    /// Read `config.toml` and change `setting` of the device at `route` in
    /// it, under the key the agent resolves that device's settings by. Also
    /// answers whether the device is online.
    async fn stage(
        &self,
        route: &DeviceRoute,
        setting: DeviceSetting,
    ) -> Result<(DeviceEdit, bool), SettingError> {
        let (stable, identity, online) = {
            let orchestrator = self.orchestrator.lock().await;
            let device = orchestrator
                .devices
                .iter()
                .find(|device| device.route.as_ref() == Some(route))
                .ok_or_else(|| SettingError::UnknownDevice(route.clone()))?;
            (
                stable_id(device),
                DeviceIdentity::from_parts(device.serial.as_deref(), device.unit_id),
                device.online,
            )
        };
        let path = self.path.clone();
        let edit = on_disk(move || {
            let path = match path {
                Some(path) => path,
                None => openlogi_core::paths::config_path().map_err(ConfigError::from)?,
            };
            // An offline probe reads no identity, as for the reapply.
            let mut edit = DeviceEdit::load(&path, &stable, online.then_some(&identity))?;
            edit.set(setting)?;
            Ok::<_, DeviceEditError>(edit)
        })
        .await?;
        Ok((edit, online))
    }

    async fn save(&self, edit: DeviceEdit) -> Result<(), SettingError> {
        on_disk(move || edit.commit())
            .await
            .map_err(SettingError::NotSaved)?;
        self.reload
            .reload_config_file()
            .await
            .map_err(SettingError::NotReloaded)
    }
}

/// Run `io` — which reads or writes `config.toml`, and may wait out another
/// writer's lock — off the async worker threads.
async fn on_disk<T: Send + 'static>(io: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(io).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}
