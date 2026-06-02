//! `openlogi diag smartshift` — SmartShift toggle round-trip.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::first_online_device;

#[derive(Debug, Args)]
pub struct SmartshiftArgs {
    /// Leave the wheel in the toggled mode (skip the second toggle that
    /// restores the original). Useful for visually verifying the flip.
    #[arg(long, conflicts_with = "sensitivity")]
    pub leave_flipped: bool,

    /// Set the auto-disengage sensitivity instead of toggling, keeping the
    /// current Free/Ratchet mode. N is 1-255 (the wheel's speed threshold to
    /// free-spin): lower = more sensitive; typical 10-40; 255 = permanent
    /// ratchet.
    #[arg(long, value_name = "N")]
    pub sensitivity: Option<u8>,
}

pub async fn run(args: SmartshiftArgs) -> Result<()> {
    let (route, name) = first_online_device().await?;
    println!("device: {name} ({route})");

    if let Some(n) = args.sensitivity {
        if n == 0 {
            anyhow::bail!("sensitivity must be 1-255 (0 means \"no change\")");
        }

        let before = openlogi_hid::get_smartshift_status(&route)
            .await
            .context("read SmartShift status")?;
        println!(
            "  current: mode={:?} sensitivity={}",
            before.mode, before.auto_disengage
        );

        let after = openlogi_hid::set_smartshift_sensitivity(&route, n)
            .await
            .context("set SmartShift sensitivity")?;
        println!(
            "  read-back: mode={:?} sensitivity={}",
            after.mode, after.auto_disengage
        );

        if after.auto_disengage != n {
            anyhow::bail!(
                "SmartShift sensitivity write not applied: requested {n}, device reports {}",
                after.auto_disengage
            );
        }
        if after.mode != before.mode {
            anyhow::bail!(
                "SmartShift mode changed unexpectedly: was {:?}, now {:?}",
                before.mode,
                after.mode
            );
        }

        println!(
            "✓ SmartShift sensitivity set to {n} (mode {:?} preserved)",
            after.mode
        );
        return Ok(());
    }

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
