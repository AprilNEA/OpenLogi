//! Agent-only automation. JSON is an external contract, independent of bincode IPC.

use std::io::{self, Write as _};
use std::num::NonZeroU8;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use openlogi_core::hid::{DeviceRoute, Dpi, ReportRate, SmartShiftMode, WriteError};
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
    /// Enable host-button events without a linked mouse (requires --save).
    HostSwitchEvents {
        /// Exact keyboard ID from api devices.
        #[arg(long)]
        device: String,
        #[arg(long, value_enum)]
        set: Switch,
    },
    /// Stream newly observed host-button choices; initial history is skipped.
    WatchHostSwitch {
        /// Exact keyboard ID from api devices.
        #[arg(long)]
        device: String,
    },
    /// Read Fn lock, or set it immediately (add --save to persist).
    FnLock {
        /// Exact opaque ID returned by `api devices`.
        #[arg(long)]
        device: String,
        /// On means bare function keys send F1–F12; off means media keys.
        #[arg(long, value_enum)]
        set: Option<Switch>,
    },
    /// Keep a keyboard on the host OS's key layout (requires --save).
    OsLayout {
        /// Exact opaque ID returned by `api devices`.
        #[arg(long)]
        device: String,
        /// On sets the host OS's layout when the keyboard connects and after it
        /// switches layout; off leaves the keyboard's layout alone.
        #[arg(long, value_enum)]
        set: Switch,
    },
    /// Read the report rate, or set it immediately (add --save to persist).
    ReportRate {
        /// Exact opaque ID returned by `api devices`.
        #[arg(long)]
        device: String,
        /// A rate in hertz that the device lists for its current connection.
        /// Omit to read only.
        #[arg(long)]
        set: Option<u16>,
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
    /// Turn the setting on.
    On,
    /// Turn the setting off.
    Off,
}

impl Switch {
    /// Whether this asks for the setting to be on.
    #[must_use]
    pub fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// Emit one JSON envelope and a matching process status, including runtime failures.
pub async fn run(args: ApiArgs) -> Result<ExitCode> {
    if let ApiCommand::WatchHostSwitch { device } = &args.command {
        if args.save {
            return emit_stream_failure("--save is not valid for observation");
        }
        return watch_host_switch(device).await;
    }
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

fn stream_line(value: &Value) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}
fn emit_stream_failure(message: &str) -> Result<ExitCode> {
    stream_line(&envelope(Err(ApiError::new(
        "event_stream_failed",
        message,
    ))))?;
    Ok(ExitCode::FAILURE)
}
async fn watch_host_switch(device: &str) -> Result<ExitCode> {
    let client = match agent::connect().await {
        Ok(client) => client,
        Err(error) => return emit_stream_failure(&error.to_string()),
    };
    let snapshot = match agent::call(client.snapshot(context::current())).await {
        Ok(value) => value,
        Err(error) => return emit_stream_failure(&format!("{error:?}")),
    };
    let route = match select_device(&snapshot, device) {
        Ok(route) => route,
        Err(error) => return emit_stream_failure(&format!("{error:?}")),
    };
    let mut observer = openlogi_ipc::client::HostSwitchEvents::new(client);
    loop {
        let observation = match observer.next().await {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(_) => {
                return emit_stream_failure("agent connection lost; history was not replayed");
            }
        };
        let choices = match observation {
            openlogi_ipc::client::HostSwitchUpdate::Baseline => {
                stream_line(&envelope(Ok(json!({"type":"baseline","device":device}))))?;
                continue;
            }
            openlogi_ipc::client::HostSwitchUpdate::Choices(choices) => choices,
        };
        for event in choices.iter().filter(|event| event.route == route) {
            stream_line(&envelope(Ok(
                json!({"type":"host_switch","device":device,"sequence":event.sequence,"channel":event.channel.number(),"observed_at_ms":event.observed_at_ms}),
            )))?;
        }
    }
}

/// Whether the agent measured the device at `route` and found that software
/// cannot set its key layout. Unmeasured capabilities are not a refusal.
fn measured_without_os_layout(snapshot: &AgentSnapshot, route: &DeviceRoute) -> bool {
    snapshot.inventory.iter().any(|inventory| {
        inventory.paired.iter().any(|paired| {
            DeviceRoute::for_slot(inventory, paired.slot).as_ref() == Some(route)
                && paired.capabilities.is_some_and(|caps| !caps.os_layout)
        })
    })
}

fn status_data(snapshot: &AgentSnapshot) -> Value {
    json!({"agent_version":snapshot.status.agent_version,"inventory":inventory_health(snapshot.status.inventory),
        "accessibility_granted":snapshot.status.accessibility_granted,"input_monitoring_granted":snapshot.status.input_monitoring_granted,
        "hook_installed":snapshot.status.hook_installed,"hid_open_failures":snapshot.status.hid_open_failures,"launch_at_login":snapshot.status.launch_at_login})
}

fn host_switch_setting(
    snapshot: &AgentSnapshot,
    device: &str,
    set: Switch,
    save: Option<&mut Save>,
) -> Result<Value, ApiError> {
    let _route = output::select_host_watch_target(snapshot, device, set)?;
    let Some(save) = save else {
        return Err(ApiError::new(
            "invalid_value",
            "host-switch-events requires --save",
        ));
    };
    save.host_switch_events(set.is_on());
    Ok(json!({"device":device,"host_switch_events":set.is_on(),"persistence":"not_saved"}))
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
        ApiCommand::Status => Ok(status_data(&snapshot)),
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
        ApiCommand::WatchHostSwitch { .. } => Err(ApiError::new(
            "invalid_value",
            "use the dedicated event stream",
        )),
        ApiCommand::HostSwitchEvents { device, set } => {
            host_switch_setting(&snapshot, &device, set, save.as_mut())
        }
        ApiCommand::FnLock { device, set } => {
            let route = select_device(&snapshot, &device)?;
            let state = match set {
                Some(set) => {
                    let requested = set.is_on();
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
        ApiCommand::OsLayout { device, set } => {
            let route = select_device(&snapshot, &device)?;
            // An agent setting, not a device state: the agent writes the
            // layout when it reloads the saved configuration.
            let Some(save) = save.as_mut() else {
                return Err(ApiError::new(
                    "invalid_value",
                    "os-layout changes an agent setting; add --save",
                ));
            };
            if measured_without_os_layout(&snapshot, &route) {
                return Err(ApiError::new(
                    "unsupported_feature",
                    "the keyboard does not let software set its key layout",
                ));
            }
            let keep = set.is_on();
            save.keep_os_layout(keep);
            Ok(json!({
                "device": device,
                "persistence": "not_saved",
                "keep_os_layout": keep,
            }))
        }
        ApiCommand::ReportRate { device, set } => {
            report_rate(
                client,
                select_device(&snapshot, &device)?,
                set,
                save.as_mut(),
            )
            .await
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

async fn report_rate(
    client: &AgentClient,
    route: DeviceRoute,
    set: Option<u16>,
    save: Option<&mut Save>,
) -> Result<Value, ApiError> {
    let device = route.to_string();
    let mut info =
        agent::call(client.read_report_rate(context::current(), route.clone())).await??;
    if let Some(value) = set {
        let requested = ReportRate::new(value);
        if !info.supports(requested) {
            return Err(ApiError::new(
                "invalid_value",
                "report rate is not one the device lists for its current connection",
            ));
        }
        if let Some(save) = save {
            save.report_rate(requested);
        }
        info = agent::call(client.set_report_rate(context::current(), route, requested)).await??;
        // A superseded agent request reads instead of writing; Ok alone does
        // not guarantee that our requested rate won.
        if info.current != requested {
            return Err(ApiError::new(
                "readback_mismatch",
                "device report rate differs from the requested value after the write",
            ));
        }
    }
    Ok(json!({
        "device": device,
        "persistence": "not_saved",
        "current": info.current,
        "supported": info.supported,
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
            WriteError::FeatureUnsupported { .. }
            | WriteError::LightUnsupported { .. }
            | WriteError::HapticWaveformUnsupported { .. } => "unsupported_feature",
            WriteError::DeviceNotFound => "device_not_found",
            WriteError::DeviceUnreachable { .. } => "device_offline",
            WriteError::AgentUnavailable => "agent_unavailable",
            WriteError::RequestTimedOut { .. } => "timeout",
            WriteError::AmbiguousRawDevice => "ambiguous_device",
            // The agent's own checks report what `dpi` checks for itself, so
            // a caller sees one code whichever side caught it.
            WriteError::DpiUnsupported { .. } | WriteError::ReportRateUnsupported { .. } => {
                "invalid_value"
            }
            WriteError::DpiNotApplied { .. } | WriteError::ReportRateNotApplied { .. } => {
                "readback_mismatch"
            }
            _ => "device_error",
        };
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests;
