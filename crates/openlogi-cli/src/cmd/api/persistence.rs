//! Stage a config edit before hardware I/O; commit only after verified readback.

use std::path::Path;

use openlogi_core::config::{Config, ConfigError, ConfigFile, SmartShift};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::{DeviceRoute, Dpi, SmartShiftStatus};
use openlogi_ipc::{AgentClient, AgentSnapshot};
use tarpc::context;

use super::{ApiCommand, ApiError, select_device};
use crate::agent;

pub(super) struct Save {
    config: Config,
    file: ConfigFile,
    key: String,
}

impl Save {
    pub(super) fn prepare(
        path: &Path,
        snapshot: &AgentSnapshot,
        command: &ApiCommand,
    ) -> Result<Self, ApiError> {
        let device = match command {
            ApiCommand::Dpi {
                device,
                set: Some(_),
            }
            | ApiCommand::FnLock {
                device,
                set: Some(_),
            } => device,
            ApiCommand::Smartshift(args)
                if args.mode.is_some() || args.auto_disengage.is_some() =>
            {
                &args.device
            }
            _ => {
                return Err(ApiError::new(
                    "invalid_value",
                    "--save requires an explicit setting change",
                ));
            }
        };
        let route = select_device(snapshot, device)?;
        let (config, file) = ConfigFile::load_from_path(path)
            .map_err(|error| ApiError::new("config_error", error.to_string()))?;
        let (slot, model) = snapshot
            .inventory
            .iter()
            .find_map(|inventory| {
                inventory.paired.iter().find_map(|paired| {
                    (DeviceRoute::for_slot(inventory, paired.slot).as_ref() == Some(&route))
                        .then_some(paired)
                        .and_then(|paired| {
                            paired.model_info.as_ref().map(|model| (paired.slot, model))
                        })
                })
            })
            .ok_or_else(|| {
                ApiError::new(
                    "identity_unavailable",
                    "saving requires a probed HID++ device identity",
                )
            })?;
        let identity = DeviceIdentity::from_parts(model.serial_number.as_deref(), model.unit_id);
        if identity.config_key().is_none() {
            return Err(ApiError::new(
                "identity_unavailable",
                "device has no persistent physical identity",
            ));
        }
        let stable = DeviceStableId::from_parts(
            Some(&route),
            slot,
            model.serial_number.as_deref(),
            model.unit_id,
        );
        let key = config
            .resolve_device_key(&stable, Some(&identity))
            .ok_or_else(|| {
                ApiError::new(
                    "identity_unavailable",
                    "device has no persistent configuration key",
                )
            })?;
        // Do not report a saved device default that an existing route override
        // would silently supersede on reconnect. Leave those preferences intact.
        if let Some(link) = config
            .devices
            .get(key.as_str())
            .and_then(|device| device.links.get(&stable.route_key()))
        {
            let overridden = match command {
                ApiCommand::Dpi { .. } => link.overrides.dpi.is_some(),
                ApiCommand::Smartshift(_) => link.overrides.smartshift.is_some(),
                _ => false,
            };
            if overridden {
                return Err(ApiError::new(
                    "link_override",
                    "this setting has a per-link override; edit it in configuration before saving a device default",
                ));
            }
        }
        Ok(Self {
            config,
            file,
            key: key.into_string(),
        })
    }

    pub(super) fn dpi(&mut self, value: Dpi) {
        self.config.set_dpi(&self.key, value);
    }

    pub(super) fn fn_lock(&mut self, value: bool) {
        self.config.set_fn_lock(&self.key, value);
    }

    pub(super) fn smartshift(&mut self, state: SmartShiftStatus) -> Result<(), ApiError> {
        if !SmartShift::accepts_auto_disengage(state.auto_disengage) {
            return Err(ApiError::new(
                "invalid_value",
                "SmartShift threshold is below the supported configuration floor; specify --auto-disengage",
            ));
        }
        self.config.set_smartshift(&self.key, state.into());
        Ok(())
    }

    pub(super) async fn commit(mut self, client: &AgentClient) -> Result<(), ApiError> {
        self.file.save(&self.config).map_err(|error| {
            let code = match &error {
                ConfigError::Conflict { .. } => "config_conflict",
                ConfigError::Write { source, .. }
                    if source.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    "config_busy"
                }
                _ => "config_save_failed",
            };
            ApiError::new(
                code,
                format!("hardware readback succeeded, but configuration was not saved: {error}"),
            )
            .after_write("not_saved")
        })?;
        match agent::call(client.reload_config(context::current())).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(ApiError::new(
                "config_reload_failed",
                format!(
                    "configuration was saved, but the agent rejected reload: {}",
                    error.message
                ),
            )
            .after_write("saved")),
            Err(error) => Err(ApiError::new(
                "config_reload_unknown",
                format!("configuration was saved, but reload acknowledgement was lost: {error:?}"),
            )
            .after_write("saved")),
        }
    }
}
