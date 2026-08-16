//! `openlogi backlight` — read and set the HID++ `0x1982` keyboard backlight.
//!
//! Unlike `diag`, this is persistent configuration: `setBacklightConfig` writes
//! to the keyboard's non-volatile memory, so `backlight off` survives
//! reconnects, host switches, and power cycles with nothing re-applying it.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use openlogi_hid::{BacklightMode, BacklightState, BacklightStatus, DeviceRoute};

use crate::cmd::diag::select_device;

/// HID++ `Backlight` — the white, level-adjustable backlight on the MX Keys
/// line. RGB keyboards use `0x8070` / `0x8080` instead and are driven by
/// `diag lighting`.
const BACKLIGHT_FEATURE: u16 = 0x1982;

#[derive(Debug, Args)]
pub struct BacklightArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// keyboards are paired.
    #[arg(long, value_name = "NAME", global = true)]
    pub device: Option<String>,

    #[command(subcommand)]
    pub action: Option<BacklightAction>,
}

#[derive(Debug, Subcommand)]
pub enum BacklightAction {
    /// Show the current backlight state (the default with no subcommand).
    Status,
    /// Turn the backlight off completely and persistently. The LEDs stay dark
    /// regardless of ambient light or hand proximity.
    Off,
    /// Turn the backlight back on, restoring the mode and brightness level the
    /// keyboard still has stored.
    On,
}

pub async fn run(args: BacklightArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[BACKLIGHT_FEATURE]).await?;
    println!("device: {name} ({route})");

    let enable = match args.action.unwrap_or(BacklightAction::Status) {
        BacklightAction::Status => {
            let state = read_state(&route).await?;
            print_state("current", state);
            return Ok(());
        }
        BacklightAction::Off => false,
        BacklightAction::On => true,
    };

    let before = read_state(&route).await?;
    print_state("current", before);

    // The keyboard sets this mode itself from its backlight keys and
    // setBacklightConfig cannot write it back, so say so rather than let the
    // mode change look like a side effect of the enable bit.
    if before.mode == BacklightMode::TemporaryManual {
        println!(
            "  note: the level came from the keyboard's backlight keys, a mode software cannot write back — it returns to automatic (ambient-light sensor)"
        );
    }

    let after = openlogi_hid::set_backlight_enabled(&route, enable)
        .await
        .with_context(|| {
            format!(
                "set backlight {}",
                if enable { "enabled" } else { "disabled" }
            )
        })?;
    print_state("read-back", after);

    if after.enabled != enable {
        anyhow::bail!(
            "backlight write not applied: requested enabled={enable}, device reports enabled={}",
            after.enabled
        );
    }

    if enable {
        println!(
            "✓ backlight enabled (level {}/{})",
            after.current_level, after.nb_levels
        );
    } else {
        println!("✓ backlight off — persisted to the keyboard, survives reconnect and power cycle");
    }
    Ok(())
}

async fn read_state(route: &DeviceRoute) -> Result<BacklightState> {
    openlogi_hid::get_backlight(route)
        .await
        .context("read backlight state")
}

fn print_state(label: &str, state: BacklightState) {
    println!(
        "  {label}: enabled={} mode={} status={} level={}/{}",
        state.enabled,
        mode_label(state.mode),
        status_label(state.status),
        state.current_level,
        state.nb_levels,
    );
}

fn mode_label(mode: BacklightMode) -> &'static str {
    match mode {
        BacklightMode::None => "none",
        BacklightMode::Automatic => "automatic (ambient-light sensor)",
        BacklightMode::TemporaryManual => "temporary manual (backlight keys)",
        BacklightMode::PermanentManual => "permanent manual (software)",
    }
}

fn status_label(status: BacklightStatus) -> &'static str {
    match status {
        BacklightStatus::DisabledBySoftware => "off (disabled by software)",
        BacklightStatus::DisabledByCriticalBattery => "off (critical battery)",
        BacklightStatus::AlsAutomatic => "on (following ambient light)",
        BacklightStatus::AlsSaturated => "off (ambient light saturated)",
        BacklightStatus::TemporaryManual => "on (level from backlight keys)",
        BacklightStatus::PermanentManual => "on (level from software)",
    }
}
