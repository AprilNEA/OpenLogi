//! Agent-only automation. JSON is an external contract, independent of bincode IPC.

use std::io::{self, Write as _};
use std::num::NonZeroU8;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use openlogi_core::hid::{DeviceRoute, Dpi, SmartShiftMode, WriteError};
use openlogi_ipc::{AgentClient, AgentSnapshot};
use serde_json::{Value, json};
use tarpc::context;

use crate::agent;

mod output;
mod persistence;
use output::{ApiError, devices, envelope, inventory_health, select_device};
use persistence::Save;

/// Options shared by the JSON automation commands.
#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Save an explicit setting change and ask the agent to reload configuration.
    #[arg(long, global = true)]
    save: bool,
    /// The operation to execute.
    #[command(subcommand)]
    command: ApiCommand,
}

/// Machine-readable commands. Writes affect live hardware; saving is opt-in.
#[derive(Debug, Subcommand)]
pub enum ApiCommand {
    /// Read agent health without enumerating hardware in this process.
    Status,
    /// List agent-known devices, opaque IDs, battery and measured capabilities.
    Devices,
    /// Read DPI, or set it immediately (add --save to persist).
    Dpi {
        /// Exact opaque ID returned by `api devices`; names are not accepted.
        #[arg(long)]
        device: String,
        /// A device-supported DPI value. Omit to read only.
        #[arg(long)]
        set: Option<u16>,
    },
    /// Read SmartShift, or change selected fields (add --save to persist).
    Smartshift(SmartshiftArgs),
    /// Read Fn lock, or set it immediately (add --save to persist).
    FnLock {
        /// Exact opaque ID returned by `api devices`.
        #[arg(long)]
        device: String,
        /// On means bare function keys send F1–F12; off means media keys.
        #[arg(long, value_enum)]
        set: Option<Switch>,
    },
}

/// A partial SmartShift update; omitted fields are read from the device.
#[derive(Debug, Args)]
pub struct SmartshiftArgs {
    /// Exact opaque ID returned by `api devices`.
    #[arg(long)]
    device: String,
    /// Wheel mode. Unspecified fields retain their current values.
    #[arg(long, value_enum)]
    mode: Option<WheelMode>,
    /// Auto-release threshold (1–254), or 255 for permanent ratchet.
    #[arg(long)]
    auto_disengage: Option<NonZeroU8>,
}

/// Explicit wheel modes accepted at the CLI boundary.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WheelMode {
    /// Free-spinning wheel.
    Free,
    /// Ratcheted wheel, subject to the auto-disengage threshold.
    Ratchet,
}

impl From<WheelMode> for SmartShiftMode {
    fn from(mode: WheelMode) -> Self {
        match mode {
            WheelMode::Free => Self::Free,
            WheelMode::Ratchet => Self::Ratchet,
        }
    }
}

/// An explicit state rather than a read/modify/write toggle.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Switch {
    /// Enable Fn lock.
    On,
    /// Disable Fn lock.
    Off,
}

/// Emit one JSON envelope and a matching process status, including runtime failures.
pub async fn run(args: ApiArgs) -> Result<ExitCode> {
    let result = match agent::connect().await {
        Ok(client) => {
            let path = args
                .save
                .then(openlogi_core::paths::config_path)
                .transpose();
            match path {
                Ok(path) => execute(&client, args.command, path.as_deref()).await,
                Err(error) => Err(ApiError::new("config_error", error.to_string())),
            }
        }
        Err(error) => Err(ApiError::from(error)),
    };
    let success = result.is_ok();
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &envelope(result))?;
    writeln!(stdout)?;
    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn execute(
    client: &AgentClient,
    command: ApiCommand,
    path: Option<&Path>,
) -> Result<Value, ApiError> {
    // Preserve typed transport failures for the JSON boundary; the human-facing
    // agent::snapshot helper intentionally turns them into prose.
    let snapshot = agent::call(client.snapshot(context::current())).await?;
    let mut save = path
        .map(|path| Save::prepare(path, &snapshot, &command))
        .transpose()?;
    let mut result = match command {
        ApiCommand::Status => Ok(json!({
            "agent_version": snapshot.status.agent_version,
            "inventory": inventory_health(snapshot.status.inventory),
            "accessibility_granted": snapshot.status.accessibility_granted,
            "input_monitoring_granted": snapshot.status.input_monitoring_granted,
            "hook_installed": snapshot.status.hook_installed,
            "hid_open_failures": snapshot.status.hid_open_failures,
            "launch_at_login": snapshot.status.launch_at_login,
        })),
        ApiCommand::Devices => Ok(json!({
            "inventory": inventory_health(snapshot.status.inventory),
            "devices": devices(&snapshot),
        })),
        ApiCommand::Dpi { device, set } => {
            dpi(
                client,
                select_device(&snapshot, &device)?,
                set,
                save.as_mut(),
            )
            .await
        }
        ApiCommand::Smartshift(args) => smartshift(client, &snapshot, &args, save.as_mut()).await,
        ApiCommand::FnLock { device, set } => {
            let route = select_device(&snapshot, &device)?;
            let state = match set {
                Some(set) => {
                    let requested = matches!(set, Switch::On);
                    if let Some(save) = save.as_mut() {
                        save.fn_lock(requested);
                    }
                    let observed =
                        agent::call(client.set_fn_lock(context::current(), route, requested))
                            .await??;
                    // A superseded agent request reads instead of writing; Ok
                    // alone does not guarantee that our requested state won.
                    if observed.fn_lock != requested {
                        return Err(ApiError::new(
                            "readback_mismatch",
                            "device Fn lock differs from the requested state after the write",
                        ));
                    }
                    observed
                }
                None => agent::call(client.read_fn_lock(context::current(), route)).await??,
            };
            Ok(json!({
                "device": device,
                "persistence": "not_saved",
                "fn_lock": state.fn_lock,
                "default_fn_lock": state.default_fn_lock,
            }))
        }
    }?;
    if let Some(save) = save {
        save.commit(client).await?;
        result["persistence"] = json!("saved");
    }
    Ok(result)
}

async fn dpi(
    client: &AgentClient,
    route: DeviceRoute,
    set: Option<u16>,
    save: Option<&mut Save>,
) -> Result<Value, ApiError> {
    let device = route.to_string();
    let mut info = agent::call(client.read_dpi(context::current(), route.clone())).await??;
    if let Some(value) = set {
        let requested = Dpi::from(value);
        if !info.capabilities.contains(requested) {
            return Err(ApiError::new(
                "invalid_value",
                "DPI is not in the device-reported supported values",
            ));
        }
        if let Some(save) = save {
            save.dpi(requested);
        }
        agent::call(client.set_dpi(context::current(), route.clone(), requested)).await??;
        info = agent::call(client.read_dpi(context::current(), route)).await??;
        if info.current != requested {
            return Err(ApiError::new(
                "readback_mismatch",
                "device DPI differs from the requested value after the write",
            ));
        }
    }
    Ok(json!({
        "device": device,
        "persistence": "not_saved",
        "current": info.current,
        "supported": info.capabilities.values(),
    }))
}

async fn smartshift(
    client: &AgentClient,
    snapshot: &AgentSnapshot,
    args: &SmartshiftArgs,
    save: Option<&mut Save>,
) -> Result<Value, ApiError> {
    let route = select_device(snapshot, &args.device)?;
    let mut state =
        agent::call(client.read_smartshift(context::current(), route.clone())).await??;
    if args.mode.is_some() || args.auto_disengage.is_some() {
        if let Some(mode) = args.mode {
            state.mode = mode.into();
        }
        if let Some(auto_disengage) = args.auto_disengage {
            state.auto_disengage = auto_disengage.into();
        }
        if let Some(save) = save {
            save.smartshift(state)?;
        }
        agent::call(client.set_smartshift(context::current(), route.clone(), state)).await??;
        let observed = agent::call(client.read_smartshift(context::current(), route)).await??;
        if observed != state {
            return Err(ApiError::new(
                "readback_mismatch",
                "device SmartShift differs from the requested state after the write",
            ));
        }
        state = observed;
    }
    Ok(json!({
        "device": args.device,
        "persistence": "not_saved",
        "mode": match state.mode {
            SmartShiftMode::Free => "free",
            SmartShiftMode::Ratchet => "ratchet",
        },
        "auto_disengage": u8::from(state.auto_disengage),
        "tunable_torque": state.tunable_torque.map(openlogi_core::hid::TunableTorque::into_inner),
    }))
}

impl From<WriteError> for ApiError {
    fn from(error: WriteError) -> Self {
        let code = match &error {
            WriteError::FeatureUnsupported { .. } | WriteError::LightUnsupported { .. } => {
                "unsupported_feature"
            }
            WriteError::DeviceNotFound => "device_not_found",
            WriteError::DeviceUnreachable { .. } => "device_offline",
            WriteError::AgentUnavailable => "agent_unavailable",
            WriteError::RequestTimedOut { .. } => "timeout",
            WriteError::AmbiguousRawDevice => "ambiguous_device",
            _ => "device_error",
        };
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests;
