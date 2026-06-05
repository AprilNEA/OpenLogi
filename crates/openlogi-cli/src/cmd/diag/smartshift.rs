//! `openlogi diag smartshift` — SmartShift toggle round-trip.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::first_online_device;

#[derive(Debug, Args)]
pub struct SmartshiftArgs {
    /// Leave the wheel in the toggled mode (skip the second toggle that
    /// restores the original). Useful for visually verifying the flip.
    #[arg(long)]
    pub leave_flipped: bool,
}

pub async fn run(args: SmartshiftArgs) -> Result<()> {
    let (route, name) = first_online_device().await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_smartshift_status(&route)
        .await
        .context("read SmartShift status")?;
    println!(
        "  current: mode={:?} auto_disengage={} torque={}",
        before.mode, before.auto_disengage, before.tunable_torque
    );

    let new_mode = openlogi_hid::toggle_smartshift(&route)
        .await
        .context("toggle SmartShift")?;
    println!("  toggled to: {new_mode:?}");

    let after = openlogi_hid::get_smartshift_status(&route)
        .await
        .context("read SmartShift after toggle")?;
    println!(
        "  read-back: mode={:?} auto_disengage={} torque={}",
        after.mode, after.auto_disengage, after.tunable_torque
    );

    if after.mode == before.mode {
        anyhow::bail!(
            "SmartShift toggle had no effect: still {:?} after write",
            before.mode
        );
    }

    if args.leave_flipped {
        println!("✓ SmartShift toggle OK (wheel left in {new_mode:?})");
        return Ok(());
    }

    println!("  restoring mode: {:?}", before.mode);
    openlogi_hid::toggle_smartshift(&route)
        .await
        .context("restore SmartShift")?;

    println!("✓ SmartShift round-trip OK");
    Ok(())
}
