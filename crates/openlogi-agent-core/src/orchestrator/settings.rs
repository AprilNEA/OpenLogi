//! Changing a device's saved settings from inside the agent, with the effect
//! a change in the settings app has: the DPI, the SmartShift wheel, Fn lock,
//! the disabled keys, a standalone light, and whether OpenLogi manages the
//! device.
//!
//! An online device takes a change first, so a value it refuses is never
//! saved and re-sent at every reconnect. `config.toml` then saves the change
//! through [`DeviceEdit`] — only while the file is still at the revision read
//! before the write — and the agent reloads the saved file through its one
//! reload path, [`ConfigReload`]. An offline device's settings are only
//! saved: the agent writes them when it reconnects.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use openlogi_core::config::{ConfigError, DeviceEdit, DeviceEditError, DeviceSetting};
use openlogi_core::device_order::DeviceIdentity;
use openlogi_hid::{DeviceRoute, DisableKeysMask, Dpi, LightCommand, SmartShiftStatus, WriteError};
use openlogi_ipc::ConfigReloadError;
use thiserror::Error;
use tokio::sync::Mutex;

use super::devices::stable_id;
use super::{Orchestrator, SharedHandles, write_light, write_manual_light_power};
use crate::hardware::known_unit;

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
    /// The agent knows no device at the route, or knows another device there
    /// than the one the change is for.
    #[error("the device is not known at {0}")]
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

/// A device at the route the agent reaches it by, told apart from another
/// device later on that route, such as one paired into the same receiver
/// slot, by the HID++ `0x0003` unit ID it reported. An all-zero unit ID,
/// which a device never probed has, tells no device apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoutedDevice {
    /// Where the agent reads and writes the device.
    pub route: DeviceRoute,
    /// Its `0x0003` unit ID.
    pub unit: [u8; 4],
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

    /// Set the DPI of the mouse `device` — one of its `dpi_presets`, or any
    /// other value it supports — and save it.
    pub async fn set_dpi(&self, device: &RoutedDevice, dpi: Dpi) -> Result<(), SettingError> {
        let (edit, online) = self.stage(device, DeviceSetting::Dpi(dpi)).await?;
        if online {
            self.reaching(device).set_dpi(&device.route, dpi).await?;
        }
        self.save(edit).await
    }

    /// Set the SmartShift wheel of the mouse `device` and save it.
    pub async fn set_smartshift(
        &self,
        device: &RoutedDevice,
        status: SmartShiftStatus,
    ) -> Result<(), SettingError> {
        let (edit, online) = self
            .stage(device, DeviceSetting::SmartShift(status.into()))
            .await?;
        if online {
            self.reaching(device)
                .set_smartshift(&device.route, status)
                .await?;
        }
        self.save(edit).await
    }

    /// Set whether OpenLogi manages `device` and save it. The reload puts it
    /// into effect: a device no longer managed is left native.
    pub async fn set_enabled(
        &self,
        device: &RoutedDevice,
        enabled: bool,
    ) -> Result<(), SettingError> {
        let (edit, _) = self.stage(device, DeviceSetting::Enabled(enabled)).await?;
        self.save(edit).await
    }

    /// Set Fn lock on the keyboard `device` and save it. A write a newer one
    /// superseded saves nothing: the newer write decides.
    pub async fn set_fn_lock(
        &self,
        device: &RoutedDevice,
        fn_lock: bool,
    ) -> Result<(), SettingError> {
        let (edit, online) = self.stage(device, DeviceSetting::FnLock(fn_lock)).await?;
        if online {
            self.reaching(device)
                .set_fn_lock(&device.route, fn_lock)
                .await?;
        }
        self.save(edit).await
    }

    /// Disable the known keys in `disabled` on the keyboard `device`, and
    /// enable the rest, then save the keys the keyboard confirms it disabled.
    /// A disabled key this version does not know stays as the keyboard has
    /// it, whatever `disabled` says of it.
    pub async fn set_disabled_keys(
        &self,
        device: &RoutedDevice,
        disabled: DisableKeysMask,
    ) -> Result<(), SettingError> {
        // The keyboard refuses a request that names a key outside the known
        // set, and keeps the unknown keys it reports itself.
        let disabled = disabled & DisableKeysMask::KNOWN;
        let (mut edit, online) = self
            .stage(device, DeviceSetting::DisabledKeys(disabled))
            .await?;
        if online {
            let confirmed = self
                .reaching(device)
                .set_disable_keys(&device.route, disabled)
                .await?;
            edit.set(DeviceSetting::DisabledKeys(
                confirmed.disabled & confirmed.supported,
            ))?;
        }
        self.save(edit).await
    }

    /// Have the standalone light `device` take `command`, and save it in the
    /// light's settings. A camera-linked light keeps its power as a manual
    /// choice until the next camera transition, as the settings app's power
    /// switch keeps it. Writes the light whether or not the agent saw it
    /// online: a light answers only while it is. A light's route names the
    /// light itself, so no other device can answer on it.
    pub async fn set_light(
        &self,
        device: &RoutedDevice,
        command: LightCommand,
    ) -> Result<(), SettingError> {
        let (edit, _) = self.stage(device, DeviceSetting::Light(command)).await?;
        let route = &device.route;
        let hardware = self.shared.hardware();
        match command {
            LightCommand::Power(enabled) if self.camera_linked(route).await => {
                write_manual_light_power(&self.orchestrator, &hardware, route, enabled).await?;
            }
            _ => {
                write_light(
                    &self.orchestrator,
                    route,
                    command,
                    hardware.apply_light(route, command),
                )
                .await?;
            }
        }
        self.save(edit).await
    }

    /// The handles a write to `device` goes through, which refuse another
    /// device on its route.
    fn reaching(&self, device: &RoutedDevice) -> SharedHandles {
        self.shared.reaching(device.unit)
    }

    /// Whether the light at `route` turns on and off with the camera.
    async fn camera_linked(&self, route: &DeviceRoute) -> bool {
        let orchestrator = self.orchestrator.lock().await;
        orchestrator
            .devices
            .iter()
            .find(|device| device.route.as_ref() == Some(route))
            .and_then(|device| orchestrator.config.light(&device.config_key))
            .is_some_and(|light| light.auto_camera)
    }

    /// Read `config.toml` and change `setting` of `device` in it, under the
    /// key the agent resolves that device's settings by. Also answers
    /// whether the device is online. A device the agent now knows by another
    /// unit ID on the route, such as one paired into the receiver slot
    /// `device` left, is not `device`; a unit ID no probe read, on either
    /// side, tells no device apart.
    async fn stage(
        &self,
        device: &RoutedDevice,
        setting: DeviceSetting,
    ) -> Result<(DeviceEdit, bool), SettingError> {
        let route = &device.route;
        let (stable, identity, online) = {
            let orchestrator = self.orchestrator.lock().await;
            let unknown = || SettingError::UnknownDevice(route.clone());
            let known = orchestrator
                .devices
                .iter()
                .find(|known| known.route.as_ref() == Some(route))
                .ok_or_else(unknown)?;
            if known_unit(device.unit)
                .zip(known_unit(known.unit_id))
                .is_some_and(|(asked, found)| asked != found)
            {
                return Err(unknown());
            }
            (
                stable_id(known),
                DeviceIdentity::from_parts(known.serial.as_deref(), known.unit_id),
                known.online,
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
