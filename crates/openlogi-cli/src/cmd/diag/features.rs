//! `openlogi diag features` — dump the device's HID++ feature table.
//!
//! Useful for figuring out *which* DPI / SmartShift / etc. feature ID a
//! given peripheral exposes when the default wrappers (0x2201, 0x2111)
//! aren't recognised.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::first_online_device;

#[derive(Debug, Args)]
pub struct FeaturesArgs {}

pub async fn run(_args: FeaturesArgs) -> Result<()> {
    let (route, name) = first_online_device().await?;
    println!("device: {name} ({route})");

    let entries = openlogi_hid::dump_features(&route)
        .await
        .context("dump features")?;

    println!("  {:>4}  {:>6}  {:<7}", "idx", "id", "ver");
    for (idx, entry) in entries.iter().enumerate() {
        println!("  {:>4}  0x{:04x}  v{}", idx, entry.id, entry.version);
    }
    println!("  ({} feature entries)", entries.len());
    Ok(())
}
