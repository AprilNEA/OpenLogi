use anyhow::{Context as _, Result, anyhow};
use clap::Args;
use tarpc::context;

use super::select_inventory_device;
use crate::agent::{self, CallFailure};

/// Arguments for a software-initiated multi-host device switch.
#[derive(Debug, Args)]
pub struct HostSwitchArgs {
    /// Device name substring from `openlogi list`.
    #[arg(long)]
    pub device: String,
    /// Zero-based host slot reported by the device (usually 0, 1, or 2).
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

    let result = agent::call(client.switch_host(context::current(), route.clone(), args.host))
        .await
        .map_err(|failure| match failure {
            CallFailure::TimedOut => {
                anyhow!("timed out waiting for the OpenLogi agent's host-switch result")
            }
            CallFailure::Disconnected => {
                anyhow!("the OpenLogi agent dropped the host-switch request")
            }
        })?;
    result.with_context(|| {
        format!(
            "could not switch {name} ({route}) to zero-based host {}",
            args.host
        )
    })?;

    println!(
        "Host switch command completed for {name} ({route}) to zero-based host {}.",
        args.host
    );
    println!(
        "The ChangeHost write is fire-and-forget; this does not confirm arrival at the destination."
    );
    Ok(())
}
