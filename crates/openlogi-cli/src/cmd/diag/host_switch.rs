//! `openlogi diag host-switch` — move one device to another Easy-Switch host.
//!
//! Unlike the other diag subcommands, this one goes through the running agent:
//! the agent owns the device's HID channel, and the device leaves this
//! computer's radio link as soon as the write takes effect.

use anyhow::{Context as _, Result, anyhow};
use clap::Args;
use tarpc::context;

use super::select_inventory_device;
use crate::agent::{self, CallFailure};

/// Arguments for a software-initiated multi-host device switch.
#[derive(Debug, Args)]
pub struct HostSwitchArgs {
    /// Device name from `openlogi list`: a case-insensitive substring that
    /// must match exactly one online device.
    #[arg(long)]
    pub device: String,
    /// Zero-based Easy-Switch slot (0, 1 or 2 on a three-channel device).
    #[arg(long)]
    pub host: u8,
}

/// Ask the running agent to switch one device to another paired host slot.
pub async fn run(args: HostSwitchArgs) -> Result<()> {
    let client = agent::connect()
        .await
        .context("could not connect to the running OpenLogi agent")?;
    let snapshot = agent::snapshot(&client).await?;
    let (route, name) = select_inventory_device(&snapshot.inventory, &args.device)?;
    let host = args.host;
    let result = agent::call(client.switch_host(context::current(), route.clone(), host))
        .await
        .map_err(|failure| {
            let what = match failure {
                CallFailure::TimedOut => "did not answer the host switch in time",
                CallFailure::Disconnected => "disconnected during the host switch",
            };
            anyhow!("the running Agent {what}; {name} may have switched anyway")
        })?;
    result.with_context(|| format!("could not switch {name} ({route}) to host {host}"))?;
    println!("Sent {name} ({route}) to zero-based host {host}.");
    println!("ChangeHost is fire-and-forget: this does not confirm that the device arrived.");
    Ok(())
}
