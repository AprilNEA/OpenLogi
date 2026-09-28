//! `openlogi fn-lock` — read and set a keyboard's Fn lock (HID++ fn inversion,
//! `0x40a3` on Easy-Switch keyboards, `0x40a2` on single-host ones).
//!
//! On `0x40a3` the state belongs to the Easy-Switch slot the keyboard is
//! talking to, so this reads and writes the current host's setting.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use openlogi_hid::DeviceRoute;

use crate::cmd::diag::select_device;

/// Fn inversion for multi-host (`0x40a3`) and single-host (`0x40a2`) boards.
const FN_INVERSION_FEATURES: [u16; 2] = [0x40a3, 0x40a2];

#[derive(Debug, Args)]
pub struct FnLockArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// keyboards are paired.
    #[arg(long, value_name = "NAME", global = true)]
    pub device: Option<String>,

    #[command(subcommand)]
    pub action: Option<FnLockAction>,
}

#[derive(Debug, Subcommand)]
pub enum FnLockAction {
    /// Show the current Fn-lock state (the default with no subcommand).
    Status,
    /// The F-row sends F1–F12; hold Fn for the printed functions.
    On,
    /// The F-row sends its printed functions; hold Fn for F1–F12.
    Off,
}

pub async fn run(args: FnLockArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &FN_INVERSION_FEATURES).await?;
    println!("device: {name} ({route})");

    let on = match args.action.unwrap_or(FnLockAction::Status) {
        FnLockAction::Status => {
            print_state("current", read_state(&route).await?);
            return Ok(());
        }
        FnLockAction::On => true,
        FnLockAction::Off => false,
    };

    openlogi_hid::set_fn_lock(&route, on)
        .await
        .with_context(|| format!("set fn lock {}", if on { "on" } else { "off" }))?;
    let after = read_state(&route).await?;
    print_state("read-back", after);
    if after != on {
        anyhow::bail!("fn-lock write not applied: requested on={on}, device reports on={after}");
    }
    Ok(())
}

async fn read_state(route: &DeviceRoute) -> Result<bool> {
    openlogi_hid::get_fn_lock(route)
        .await
        .context("read fn-lock state")
}

fn print_state(label: &str, on: bool) {
    let meaning = if on {
        "on (F-row sends F1–F12; Fn gives the printed functions)"
    } else {
        "off (F-row sends the printed functions; Fn gives F1–F12)"
    };
    println!("  {label}: {meaning}");
}
