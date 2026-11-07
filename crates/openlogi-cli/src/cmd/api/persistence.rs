//! Stage a config edit before hardware I/O; commit only after verified readback.

use std::path::Path;

use openlogi_core::config::{ConfigError, DeviceEdit, DeviceEditError, DeviceSetting};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_core::hid::{DeviceRoute, Dpi, ReportRate, SmartShiftStatus};
use openlogi_ipc::{AgentClient, AgentSnapshot};
use tarpc::context;

use super::{ApiCommand, ApiError, select_device};
use crate::agent;

pub(super) struct Save(DeviceEdit);

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
            }
            | ApiCommand::ReportRate {
                device,
                set: Some(_),
            }
            | ApiCommand::OsLayout { device, .. } => device,
            ApiCommand::Smartshift(args)
                if args.mode.is_some() || args.auto_disengage.is_some() =>
            {
                &args.device
            }
            ApiCommand::EasySwitch { .. } => {
                return Err(ApiError::new(
                    "invalid_value",
                    "the device keeps Easy-Switch names itself; drop --save",
                ));
            }
            _ => {
                return Err(ApiError::new(
                    "invalid_value",
                    "--save requires an explicit setting change",
                ));
            }
        };
        let route = select_device(snapshot, device)?;
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
        let mut edit = DeviceEdit::load(path, &stable, Some(&identity)).map_err(refused)?;
        // Stage the DPI the command names now, so a per-link override refuses
        // it before any hardware I/O.
        if let ApiCommand::Dpi {
            set: Some(value), ..
        } = command
        {
            edit.set(DeviceSetting::Dpi(Dpi::from(*value)))
                .map_err(refused)?;
        }
        Ok(Self(edit))
    }

    pub(super) fn fn_lock(&mut self, value: bool) -> Result<(), ApiError> {
        self.0.set(DeviceSetting::FnLock(value)).map_err(refused)
    }

    pub(super) fn keep_os_layout(&mut self, keep: bool) -> Result<(), ApiError> {
        self.0
            .set(DeviceSetting::KeepOsLayout(keep))
            .map_err(refused)
    }

    pub(super) fn report_rate(&mut self, rate: ReportRate) -> Result<(), ApiError> {
        self.0.set(DeviceSetting::ReportRate(rate)).map_err(refused)
    }

    pub(super) fn smartshift(&mut self, state: SmartShiftStatus) -> Result<(), ApiError> {
        self.0
            .set(DeviceSetting::SmartShift(state.into()))
            .map_err(refused)
    }

    pub(super) async fn commit(self, client: &AgentClient) -> Result<(), ApiError> {
        self.0.commit().map_err(|error| {
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

/// The JSON error for a setting the configuration cannot take.
fn refused(error: DeviceEditError) -> ApiError {
    match error {
        DeviceEditError::Config(error) => ApiError::new("config_error", error.to_string()),
        DeviceEditError::NoConfigKey => ApiError::new("identity_unavailable", error.to_string()),
        DeviceEditError::LinkOverride => ApiError::new("link_override", error.to_string()),
        DeviceEditError::SmartShiftThreshold => ApiError::new(
            "invalid_value",
            "SmartShift threshold is below the supported configuration floor; specify --auto-disengage",
        ),
    }
}
