//! `openlogi diag hidden` — read or set the `0x1e00 EnableHiddenFeatures`
//! gate. Some devices keep auxiliary event sources dormant until a host
//! enables this; the setting does not survive a device power cycle.

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct HiddenArgs {
    /// Enable hidden features (write `1`).
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,

    /// Disable hidden features (write `0`).
    #[arg(long)]
    pub disable: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: HiddenArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x1e00]).await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::hidden_features::hidden_features_enabled(&route)
        .await
        .context("read 0x1e00 enabled state")?;
    println!("hidden features enabled: {before}");

    let target = match (args.enable, args.disable) {
        (true, _) => true,
        (_, true) => false,
        _ => return Ok(()),
    };

    let after = openlogi_hid::hidden_features::set_hidden_features_enabled(&route, target)
        .await
        .context("write 0x1e00 enabled state")?;
    println!("hidden features enabled (read-back): {after}");
    if after != target {
        bail!("write not applied: requested {target}, device reports {after}");
    }
    Ok(())
}
