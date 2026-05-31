//! `openlogi diag dpi` — DPI write round-trip.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::first_online_device;

#[derive(Debug, Args)]
pub struct DpiArgs {
    /// DPI to set during the test. Default = current + 200, clamped to the
    /// 200–6400 window the GUI slider uses.
    #[arg(long)]
    pub target: Option<u16>,
}

pub async fn run(args: DpiArgs) -> Result<()> {
    let (route, name) = first_online_device().await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_dpi(&route)
        .await
        .context("read current DPI")?;
    println!("  current DPI: {before}");

    let target = args.target.unwrap_or_else(|| {
        if before < 3200 {
            before.saturating_add(200).clamp(200, 6400)
        } else {
            before.saturating_sub(200).clamp(200, 6400)
        }
    });
    if target == before {
        println!(
            "  target {target} equals current — pick a different --target to exercise the write"
        );
        return Ok(());
    }

    println!("  writing DPI: {target}");
    openlogi_hid::set_dpi(&route, target)
        .await
        .context("write DPI")?;

    let after = openlogi_hid::get_dpi(&route)
        .await
        .context("read DPI after write")?;
    println!("  read-back DPI: {after}");

    if after != target {
        anyhow::bail!(
            "DPI write failed: requested {target}, device reports {after} \
             (likely out of the device's supported range)"
        );
    }

    println!("  restoring DPI: {before}");
    openlogi_hid::set_dpi(&route, before)
        .await
        .context("restore DPI")?;

    println!("✓ DPI round-trip OK");
    Ok(())
}
