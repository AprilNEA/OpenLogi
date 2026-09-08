//! `openlogi diag power-mode` — 0x8090 performance/endurance round-trip.

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use openlogi_hid::PowerMode;

use crate::cmd::diag::select_device;

/// CLI spelling of [`PowerMode`], so clap owns the value parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PowerModeArg {
    /// Full report-rate range, higher battery drain.
    Performance,
    /// Slowest report rate only, months of battery life.
    Endurance,
}

impl From<PowerModeArg> for PowerMode {
    fn from(arg: PowerModeArg) -> Self {
        match arg {
            PowerModeArg::Performance => Self::Performance,
            PowerModeArg::Endurance => Self::Endurance,
        }
    }
}

#[derive(Debug, Args)]
pub struct PowerModeArgs {
    /// Set the mode and leave it (skip the toggle round-trip). The device
    /// persists the mode across power cycles.
    #[arg(long, value_enum, value_name = "MODE")]
    pub set: Option<PowerModeArg>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: PowerModeArgs) -> Result<()> {
    // 0x8090 = ModeStatus — auto-skip devices that do not expose it.
    let (route, name) = select_device(args.device.as_deref(), &[0x8090]).await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_power_mode(&route)
        .await
        .context("read power mode")?;
    println!(
        "  current: mode={:?} software_switch={} hardware_switch={}",
        before.mode, before.software_switch, before.hardware_switch
    );

    if let Some(target) = args.set {
        let target = PowerMode::from(target);
        openlogi_hid::set_power_mode(&route, target)
            .await
            .context("set power mode")?;
        let after = openlogi_hid::get_power_mode(&route)
            .await
            .context("read power mode after set")?;
        println!("  read-back: mode={:?}", after.mode);
        if after.mode != target {
            anyhow::bail!(
                "power-mode write not applied: requested {target:?}, device reports {:?}",
                after.mode
            );
        }
        println!("✓ power mode set to {target:?}");
        return Ok(());
    }

    let flipped = before.mode.flipped();
    openlogi_hid::set_power_mode(&route, flipped)
        .await
        .context("toggle power mode")?;

    // From here on the device may hold the flipped mode, and unlike the other
    // diags' targets this setting persists across power cycles — so the
    // restore write runs on every path, including a failed or unexpected
    // confirming read.
    let verified = verify_toggle(&route, flipped).await;

    println!("  restoring mode: {:?}", before.mode);
    let restored = restore_and_verify(&route, before.mode).await;

    match (verified, restored) {
        (Ok(()), Ok(())) => {
            println!("✓ power-mode round-trip OK");
            Ok(())
        }
        // The toggle itself misbehaved but the device is back in its original
        // mode — report the diagnostic failure.
        (Err(verify), Ok(())) => Err(verify),
        (Ok(()), Err(restore)) => Err(restore),
        // Both failed: the stranded device is the more urgent fact; carry the
        // verification failure along instead of dropping it.
        (Err(verify), Err(restore)) => Err(restore.context(format!("after: {verify:#}"))),
    }
}

/// Write `mode` back and read it out again. An acknowledged restore that did
/// not apply must fail the diagnostic instead of printing a green round-trip,
/// exactly like the forward toggle.
async fn restore_and_verify(route: &openlogi_hid::DeviceRoute, mode: PowerMode) -> Result<()> {
    openlogi_hid::set_power_mode(route, mode)
        .await
        .context("restore power mode")?;
    let after = openlogi_hid::get_power_mode(route)
        .await
        .context("read power mode after restore")?;
    if after.mode != mode {
        anyhow::bail!(
            "power-mode restore not applied: device reports {:?}",
            after.mode
        );
    }
    Ok(())
}

/// Read the mode back after the toggle write and check it took.
async fn verify_toggle(route: &openlogi_hid::DeviceRoute, expected: PowerMode) -> Result<()> {
    let after = openlogi_hid::get_power_mode(route)
        .await
        .context("read power mode after toggle")?;
    println!("  toggled to: {:?}", after.mode);
    if after.mode != expected {
        anyhow::bail!(
            "power-mode toggle had no effect: still {:?} after write",
            after.mode
        );
    }
    Ok(())
}
