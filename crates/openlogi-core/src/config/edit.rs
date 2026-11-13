//! Saving a change to `config.toml` from a writer other than the settings
//! app.
//!
//! The settings app keeps one [`ConfigFile`] for as long as it runs. Every
//! other writer — `openlogi api --save`, Flow pairing, and the agent's own
//! device settings — reads the file, changes it, and saves it here: only
//! while the file is still at the revision it read, so another writer's edit
//! is never overwritten, and only after waiting out a save another writer
//! is in the middle of.

use std::io;
use std::path::Path;
use std::time::Duration;

use thiserror::Error;

use super::{Config, ConfigError, ConfigFile, ConfigRevision, DisableKey, SmartShift};
use crate::device_order::{DeviceIdentity, DeviceStableId};
use crate::hid::{DisableKeysMask, Dpi, LightCommand, ReportRate};

/// How many times a save waits for another writer to release the lock.
const LOCK_WAITS: u32 = 5;
/// How long each of those waits lasts.
const LOCK_WAIT: Duration = Duration::from_millis(20);

/// `config.toml` read for an edit, together with the revision it was read at.
#[derive(Debug)]
pub struct ConfigEdit {
    config: Config,
    file: ConfigFile,
}

impl ConfigEdit {
    /// Read the config at `path` for an edit. A missing file reads as the
    /// default config.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let (config, file) = ConfigFile::load_from_path(path)?;
        Ok(Self { config, file })
    }

    /// The config as read, with the changes made to it so far.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Change the config. Nothing reaches the file before [`Self::commit`].
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// Save the changed config, and answer the revision saved.
    ///
    /// A file that changed since it was read is a
    /// [`ConfigError::Conflict`]: it keeps the other writer's edit, and the
    /// caller decides whether to read it again. A save another writer is in
    /// the middle of is waited out first, for about a tenth of a second; one
    /// that holds the lock longer is a [`ConfigError::Write`] of kind
    /// [`io::ErrorKind::WouldBlock`]. Blocks on the disk.
    pub fn commit(mut self) -> Result<ConfigRevision, ConfigError> {
        let mut waits = 0;
        loop {
            match self.file.save(&self.config) {
                Ok(()) => return Ok(self.file.revision()),
                Err(ConfigError::Write { source, .. })
                    if source.kind() == io::ErrorKind::WouldBlock && waits < LOCK_WAITS =>
                {
                    std::thread::sleep(LOCK_WAIT);
                    waits += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// One saved setting of one device, for [`DeviceEdit::set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceSetting {
    /// [`DeviceConfig::dpi`](super::DeviceConfig::dpi).
    Dpi(Dpi),
    /// [`DeviceConfig::smartshift`](super::DeviceConfig::smartshift).
    SmartShift(SmartShift),
    /// Whether OpenLogi manages the device:
    /// [`DeviceConfig::enabled`](super::DeviceConfig::enabled).
    Enabled(bool),
    /// [`DeviceConfig::fn_lock`](super::DeviceConfig::fn_lock).
    FnLock(bool),
    /// [`DeviceConfig::keep_os_layout`](super::DeviceConfig::keep_os_layout).
    KeepOsLayout(bool),
    /// [`DeviceConfig::report_rate`](super::DeviceConfig::report_rate).
    ReportRate(ReportRate),
    /// The known keys a keyboard disables:
    /// [`DeviceConfig::disabled_keys`](super::DeviceConfig::disabled_keys),
    /// from the mask the keyboard confirmed.
    DisabledKeys(DisableKeysMask),
    /// A command a standalone light took, kept in its
    /// [`DeviceConfig::light`](super::DeviceConfig::light) settings. A light
    /// with no saved settings keeps none: the agent applies nothing to it when
    /// it reconnects, so the light holds the command on its own.
    Light(LightCommand),
}

/// Why a device setting cannot be saved.
#[derive(Debug, Error)]
pub enum DeviceEditError {
    /// Reading `config.toml` failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The device has no key its settings could be saved under.
    #[error("device has no persistent configuration key")]
    NoConfigKey,
    /// The link the device is reached on overrides the setting, so a device
    /// default would not take effect when it reconnects.
    #[error(
        "this setting has a per-link override; edit it in configuration before saving a device default"
    )]
    LinkOverride,
    /// The SmartShift threshold is below the floor a config can hold
    /// ([`SmartShift::accepts_auto_disengage`]).
    #[error("SmartShift threshold is below the supported configuration floor")]
    SmartShiftThreshold,
}

/// A change to one device's saved settings: a [`ConfigEdit`] for the key the
/// device's settings are saved under.
#[derive(Debug)]
pub struct DeviceEdit {
    edit: ConfigEdit,
    key: String,
    link: String,
}

impl DeviceEdit {
    /// Read the config at `path` for a change to the device reached as
    /// `stable`, whose probe read `identity` — `None` while it is offline.
    /// The settings go where the agent reads them for that device
    /// ([`Config::resolve_device_key`]).
    pub fn load(
        path: &Path,
        stable: &DeviceStableId,
        identity: Option<&DeviceIdentity>,
    ) -> Result<Self, DeviceEditError> {
        let mut edit = ConfigEdit::load(path)?;
        let key = edit
            .config
            .resolve_device_key(stable, identity)
            .ok_or(DeviceEditError::NoConfigKey)?;
        let link = stable.route_key();
        // An online device's settings go under its own identity, which only
        // the route index leads back to while it sleeps. Record the route
        // there, as the settings app does when it sees the device, so a save
        // made while it sleeps reaches the same settings.
        if identity.is_some() && key.as_str() != link {
            edit.config.adopt_route(&key, &link, None);
        }
        Ok(Self {
            edit,
            key: key.into_string(),
            link,
        })
    }

    /// Change `setting`. A device-wide DPI or SmartShift that the device's
    /// current link overrides is refused, since the override would replace
    /// it at the next reconnect, and so is a SmartShift threshold no config
    /// can hold.
    pub fn set(&mut self, setting: DeviceSetting) -> Result<(), DeviceEditError> {
        let config = &mut self.edit.config;
        let overrides = config
            .devices
            .get(&self.key)
            .and_then(|device| device.links.get(&self.link))
            .map(|link| &link.overrides);
        let overridden = match setting {
            DeviceSetting::Dpi(_) => overrides.is_some_and(|overrides| overrides.dpi.is_some()),
            DeviceSetting::SmartShift(_) => {
                overrides.is_some_and(|overrides| overrides.smartshift.is_some())
            }
            DeviceSetting::Enabled(_)
            | DeviceSetting::FnLock(_)
            | DeviceSetting::KeepOsLayout(_)
            | DeviceSetting::ReportRate(_)
            | DeviceSetting::DisabledKeys(_)
            | DeviceSetting::Light(_) => false,
        };
        if overridden {
            return Err(DeviceEditError::LinkOverride);
        }
        let key = &self.key;
        match setting {
            DeviceSetting::Dpi(dpi) => config.set_dpi(key, dpi),
            DeviceSetting::SmartShift(smartshift) => {
                if !SmartShift::accepts_auto_disengage(smartshift.auto_disengage) {
                    return Err(DeviceEditError::SmartShiftThreshold);
                }
                config.set_smartshift(key, smartshift);
            }
            DeviceSetting::Enabled(enabled) => config.set_device_enabled(key, enabled),
            DeviceSetting::FnLock(fn_lock) => config.set_fn_lock(key, fn_lock),
            DeviceSetting::KeepOsLayout(keep) => config.set_keep_os_layout(key, keep),
            DeviceSetting::ReportRate(rate) => config.set_report_rate(key, rate),
            DeviceSetting::DisabledKeys(mask) => {
                config.set_disabled_keys(key, DisableKey::disabled_in(mask));
            }
            DeviceSetting::Light(command) => {
                if let Some(mut light) = config.light(key) {
                    light.apply(command);
                    config.set_light(key, light);
                }
            }
        }
        Ok(())
    }

    /// Save the change; see [`ConfigEdit::commit`].
    pub fn commit(self) -> Result<ConfigRevision, ConfigError> {
        self.edit.commit()
    }
}
