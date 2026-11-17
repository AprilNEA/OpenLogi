//! RGB keyboard automation reads saved configuration, never invented readback.

use std::path::Path;

use clap::{Args, ValueEnum};
use openlogi_core::{color::Rgb, config::Lighting, device::DeviceKind, hid::DeviceRoute};
use openlogi_ipc::{AgentClient, AgentSnapshot};
use serde_json::{Value, json};
use tarpc::context;

use super::{ApiError, Save, select_device};
use crate::agent;

/// Partial RGB changes; omitted fields keep their saved values.
#[derive(Debug, Args)]
pub struct LightingArgs {
    /// Exact opaque ID from `api devices`.
    #[arg(long)]
    pub(super) device: String,
    /// Report saved settings without changing configuration or hardware (the default).
    #[arg(long, conflicts_with_all = ["set", "color", "brightness"])]
    pub(super) status: bool,
    /// Change the saved enabled flag, retaining colour and brightness.
    #[arg(long, value_enum)]
    pub(super) set: Option<Power>,
    /// Static colour as exactly six hexadecimal digits, without '#'.
    #[arg(long = "colour", visible_alias = "color")]
    pub(super) color: Option<Rgb>,
    /// Brightness percent (0–100); does not implicitly enable lighting.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub(super) brightness: Option<u8>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum Power {
    On,
    Off,
    Toggle,
}

struct LightingChanges {
    enabled: Option<Power>,
    color: Option<Rgb>,
    brightness: Option<u8>,
}

enum LightingOperation {
    Read,
    Change(LightingChanges),
}

impl LightingArgs {
    pub(super) fn validate(&self, persistence: Persistence) -> Result<(), ApiError> {
        self.operation(persistence).map(|_| ())
    }

    fn operation(&self, persistence: Persistence) -> Result<LightingOperation, ApiError> {
        let changes = LightingChanges {
            enabled: self.set,
            color: self.color,
            brightness: self.brightness,
        };
        let has_update =
            changes.enabled.is_some() || changes.color.is_some() || changes.brightness.is_some();
        match (self.status, has_update, persistence) {
            (_, false, Persistence::Read) => Ok(LightingOperation::Read),
            (false, true, Persistence::Save) => Ok(LightingOperation::Change(changes)),
            _ => Err(ApiError::new(
                "invalid_value",
                "lighting changes require --save; --status is read-only; omit --save to read saved settings",
            )),
        }
    }
}

impl LightingChanges {
    fn apply(&self, lighting: &mut Lighting) {
        if let Some(enabled) = self.enabled {
            lighting.enabled = match enabled {
                Power::On => true,
                Power::Off => false,
                Power::Toggle => !lighting.enabled,
            };
        }
        if let Some(color) = self.color {
            lighting.color = color;
        }
        if let Some(brightness) = self.brightness {
            lighting.brightness = brightness;
        }
    }
}

pub(super) async fn run(
    client: &AgentClient,
    snapshot: &AgentSnapshot,
    args: &LightingArgs,
    save_path: Option<&Path>,
) -> Result<Value, ApiError> {
    let default_path;
    let path = if let Some(path) = save_path {
        path
    } else {
        default_path = openlogi_core::paths::config_path()
            .map_err(|error| ApiError::new("config_error", error.to_string()))?;
        &default_path
    };
    let persistence = if save_path.is_some() {
        Persistence::Save
    } else {
        Persistence::Read
    };
    run_at(client, snapshot, args, path, persistence).await
}

/// Whether this invocation may persist a change.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Persistence {
    Read,
    Save,
}

/// Execute against one configuration path; tests use isolated temporary files.
pub(super) async fn run_at(
    client: &AgentClient,
    snapshot: &AgentSnapshot,
    args: &LightingArgs,
    path: &Path,
    persistence: Persistence,
) -> Result<Value, ApiError> {
    let route = select_device(snapshot, &args.device)?;
    let supported = snapshot.inventory.iter().any(|inventory| {
        inventory.paired.iter().any(|paired| {
            DeviceRoute::for_slot(inventory, paired.slot).as_ref() == Some(&route)
                && paired.kind == DeviceKind::Keyboard
                && paired.capabilities.is_some_and(|caps| caps.lighting)
        })
    });
    if !supported {
        return Err(ApiError::new(
            "unsupported_feature",
            "selected device has no measured RGB keyboard lighting support",
        ));
    }
    let operation = args.operation(persistence)?;
    let mut edit = Save::for_device(path, snapshot, &args.device)?;
    let configured = edit.lighting().is_some();
    let mut lighting = edit.lighting().cloned().unwrap_or_default();
    let (configured, persistence) = match operation {
        LightingOperation::Change(changes) => {
            changes.apply(&mut lighting);
            edit.set_lighting(lighting.clone())?;
            // As in the GUI: save and reload before the write-only lighting RPC.
            // A conflict must fail before we change hardware.
            let revision = edit.commit(client).await?;
            agent::call(client.set_saved_lighting(
                context::current(),
                route,
                lighting.clone(),
                revision,
            ))
            .await
            .map_err(|error| ApiError::from(error).after_write("saved"))?
            .map_err(|error| ApiError::from(error).after_write("saved"))?;
            (true, "saved")
        }
        LightingOperation::Read => (configured, "not_saved"),
    };
    Ok(json!({
        "device": args.device,
        "persistence": persistence,
        "source": "configuration",
        "configured": configured,
        "enabled": lighting.enabled,
        "color": lighting.color,
        "brightness": lighting.brightness,
    }))
}
