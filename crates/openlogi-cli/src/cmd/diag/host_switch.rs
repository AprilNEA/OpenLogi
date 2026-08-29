use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::Args;
use openlogi_ipc::{ClientKind, PROTOCOL_VERSION, client};
use tarpc::context;

use super::select_inventory_device;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const SWITCH_TIMEOUT: Duration = Duration::from_secs(7);

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
    let connection = tokio::time::timeout(CONNECT_TIMEOUT, client::connect())
        .await
        .context("timed out connecting to the OpenLogi agent")?
        .context("could not connect to the running OpenLogi agent")?;
    if connection.version != PROTOCOL_VERSION {
        bail!(
            "the agent speaks protocol v{}, but this CLI expects v{PROTOCOL_VERSION}; restart OpenLogi so both binaries match",
            connection.version
        );
    }

    tokio::time::timeout(
        CONNECT_TIMEOUT,
        connection
            .client
            .declare_client(context::current(), ClientKind::Cli),
    )
    .await
    .context("timed out declaring the CLI to the OpenLogi agent")?
    .context("the OpenLogi agent dropped the CLI declaration")?;

    let snapshot = tokio::time::timeout(
        SNAPSHOT_TIMEOUT,
        connection.client.snapshot(context::current()),
    )
    .await
    .context("timed out reading the OpenLogi agent's device inventory")?
    .context("the OpenLogi agent dropped the inventory request")?;
    let (route, name) = select_inventory_device(snapshot.inventory, &args.device)?;

    let result = tokio::time::timeout(
        SWITCH_TIMEOUT,
        connection
            .client
            .switch_host(context::current(), route.clone(), args.host),
    )
    .await
    .context("timed out waiting for the OpenLogi agent's host-switch result")?
    .context("the OpenLogi agent dropped the host-switch request")?;
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
