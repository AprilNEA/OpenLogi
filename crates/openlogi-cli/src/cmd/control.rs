//! Agent-only settings for local integrations. No direct-HID fallback, config
//! file edits, or automatic retries: an unavailable agent is an explicit error.

use std::num::NonZeroU16;

use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use openlogi_core::device::{BatteryInfo, Capabilities, DeviceInventory};
use openlogi_core::hid::{
    DeviceRoute, Dpi, SmartShiftAutoDisengage, SmartShiftChange, SmartShiftMode,
};
use openlogi_ipc::{AgentClient, InventoryHealth};
use serde::Serialize;
use serde_json::{Value, json};
use tarpc::context;

use super::target_selection::{self, DeviceTarget};
use crate::agent;

/// Read or change mouse settings through the compatible, already-running agent.
#[derive(Debug, Subcommand)]
pub enum ControlCmd {
    /// List the agent's addressable devices, capabilities and last battery reading.
    Devices,
    /// Read current/supported DPI, or set it once and verify the read-back.
    Dpi(DpiArgs),
    /// Read SmartShift, or change its mode/threshold while retaining wheel torque.
    Smartshift(SmartshiftArgs),
}

/// Select exactly one device by its full name or rendered route.
#[derive(Debug, Args)]
pub struct TargetArgs {
    /// Exact display name (case-insensitive) or exact route from control devices.
    #[arg(long)]
    device: String,
}

/// A DPI query or explicit write. Omitting set never writes.
#[derive(Debug, Args)]
pub struct DpiArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// Temporary sensor DPI; must be in the device's advertised list.
    #[arg(long, value_name = "DPI")]
    set: Option<NonZeroU16>,
}

/// Which mechanical wheel mode to request.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Mode {
    /// Free-spinning wheel.
    Free,
    /// Ratcheted wheel with the current or explicitly requested threshold.
    Ratchet,
}

/// A SmartShift query or partial edit. Unspecified fields are preserved.
#[derive(Debug, Args)]
pub struct SmartshiftArgs {
    #[command(flatten)]
    target: TargetArgs,
    /// Temporary wheel mode.
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    /// Automatic disengage threshold, 1–254; 255 means permanent ratchet.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..))]
    threshold: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
struct Device {
    route: DeviceRoute,
    id: String,
    name: String,
    online: bool,
    battery: Option<BatteryInfo>,
    capabilities: Option<Capabilities>,
}
impl DeviceTarget for Device {
    fn route(&self) -> &DeviceRoute {
        &self.route
    }
    fn display_name(&self) -> &str {
        &self.name
    }
}

fn devices(inventory: &[DeviceInventory]) -> Vec<Device> {
    inventory
        .iter()
        .flat_map(|inventory| {
            inventory.paired.iter().filter_map(|device| {
                let route = DeviceRoute::for_slot(inventory, device.slot)?;
                Some(Device {
                    id: route.to_string(),
                    route,
                    name: device
                        .codename
                        .clone()
                        .unwrap_or_else(|| "Unknown device".into()),
                    online: device.online,
                    battery: device.battery.clone(),
                    capabilities: device.capabilities,
                })
            })
        })
        .collect()
}

fn selected(inventory: &[DeviceInventory], query: &str) -> Result<Device> {
    // Keep offline/duplicate candidates in selection so an unavailable target
    // cannot silently redirect a write to another device with the same name.
    let device = target_selection::select_target(&devices(inventory), Some(query))?;
    if !device.online {
        bail!("the selected device is offline; nothing was written");
    }
    Ok(device)
}

fn read_failure(failure: agent::CallFailure) -> anyhow::Error {
    anyhow!("agent read failed ({failure:?}); this command did not attempt a write")
}

fn write_failure(failure: agent::CallFailure) -> anyhow::Error {
    anyhow!(
        "agent request failed ({failure:?}); no retry was attempted; a started write may have taken effect"
    )
}

/// Execute one command and emit a versioned JSON document on success.
/// Errors go to stderr with a failing exit status; no direct device fallback.
pub async fn run(command: ControlCmd) -> Result<()> {
    let client = agent::connect()
        .await
        .map_err(|error| anyhow!("a compatible running OpenLogi agent is required: {error}"))?;
    let result = execute(&client, command).await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

async fn execute(client: &AgentClient, command: ControlCmd) -> Result<Value> {
    let snapshot = agent::snapshot(client).await?;
    if let ControlCmd::Devices = command {
        return Ok(
            json!({"schema_version": 1, "agent_version": snapshot.status.agent_version,
            "protocol_version": snapshot.status.protocol_version, "inventory_health": snapshot.status.inventory,
            "devices": devices(&snapshot.inventory)}),
        );
    }
    if snapshot.status.inventory != InventoryHealth::Ready {
        bail!("the agent inventory is not ready; nothing was written");
    }
    match command {
        ControlCmd::Devices => unreachable!(),
        ControlCmd::Dpi(args) => {
            let device = selected(&snapshot.inventory, &args.target.device)?;
            let before = agent::call(client.read_dpi(context::current(), device.route.clone()))
                .await
                .map_err(read_failure)??;
            let Some(value) = args.set else {
                return Ok(
                    json!({"schema_version":1,"device":device.id,"written":false,"current":before}),
                );
            };
            let requested = Dpi::new(value.get());
            if !before.capabilities.contains(requested) {
                bail!("requested DPI is not advertised by the device; nothing was written");
            }
            agent::call(client.set_dpi(context::current(), device.route.clone(), requested))
                .await
                .map_err(write_failure)??;
            let after = agent::call(client.read_dpi(context::current(), device.route))
                .await
                .map_err(write_failure)??;
            if after.current != requested {
                bail!("DPI write was acknowledged but read-back differs; no retry was attempted");
            }
            Ok(
                json!({"schema_version":1,"device":device.id,"written":true,"before":before,"current":after,"persistence":"temporary"}),
            )
        }
        ControlCmd::Smartshift(args) => {
            let device = selected(&snapshot.inventory, &args.target.device)?;
            let before =
                agent::call(client.read_smartshift(context::current(), device.route.clone()))
                    .await
                    .map_err(read_failure)??;
            let requested = SmartShiftChange {
                mode: args.mode.map(|mode| match mode {
                    Mode::Free => SmartShiftMode::Free,
                    Mode::Ratchet => SmartShiftMode::Ratchet,
                }),
                auto_disengage: args
                    .threshold
                    .map(SmartShiftAutoDisengage::try_from)
                    .transpose()?,
            };
            if args.mode.is_none() && args.threshold.is_none() {
                return Ok(
                    json!({"schema_version":1,"device":device.id,"written":false,"current":before}),
                );
            }
            let after =
                agent::call(client.update_smartshift(context::current(), device.route, requested))
                    .await
                    .map_err(write_failure)??;
            if !requested.matches(after) {
                bail!(
                    "SmartShift write was acknowledged but read-back differs; no retry was attempted"
                );
            }
            Ok(
                json!({"schema_version":1,"device":device.id,"written":true,"before":before,"current":after,"persistence":"temporary"}),
            )
        }
    }
}

#[cfg(test)]
mod tests;
