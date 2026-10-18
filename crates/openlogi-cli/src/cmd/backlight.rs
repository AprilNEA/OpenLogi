//! `openlogi backlight` — read and set the HID++ `0x1982` keyboard backlight.
//!
//! Unlike `diag`, this is persistent configuration: `setBacklightConfig` writes
//! to the keyboard's non-volatile memory, so every change here survives
//! reconnects, host switches, and power cycles with nothing re-applying it.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use openlogi_core::hid::backlight::max_backlight_level;
use openlogi_hid::{
    BacklightChange, BacklightControls, BacklightMode, BacklightState, BacklightStatus, DeviceRoute,
};

use crate::cmd::api::Switch;
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
    /// Show the current backlight state and what this keyboard lets software
    /// change (the default with no subcommand).
    Status,
    /// Turn the backlight off completely and persistently. The LEDs stay dark
    /// regardless of ambient light or hand proximity.
    Off,
    /// Turn the backlight back on, restoring the mode and brightness level the
    /// keyboard still has stored.
    On,
    /// Let the ambient-light sensor choose the brightness (`on`), or hold the
    /// brightness the keyboard shows now (`off`).
    Auto {
        #[arg(value_enum)]
        set: Switch,
    },
    /// Hold this brightness level, 0 (off) up to the keyboard's level count
    /// minus one. Turns automatic brightness off.
    Level {
        #[arg(value_name = "LEVEL")]
        level: u8,
    },
    /// Let the keyboard turn the backlight off when its battery runs low.
    BatterySaving {
        #[arg(value_enum)]
        set: Switch,
    },
}

impl BacklightAction {
    /// The change this action writes, or `None` for a read.
    fn change(&self) -> Option<BacklightChange> {
        match *self {
            Self::Status => None,
            Self::Off => Some(BacklightChange::Enabled(false)),
            Self::On => Some(BacklightChange::Enabled(true)),
            Self::Auto { set } => Some(BacklightChange::Automatic(set.is_on())),
            Self::Level { level } => Some(BacklightChange::Level(level)),
            Self::BatterySaving { set } => Some(BacklightChange::PowerSave(set.is_on())),
        }
    }
}

pub async fn run(args: BacklightArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[BACKLIGHT_FEATURE]).await?;
    println!("device: {name} ({route})");

    let before = read_state(&route).await?;
    print_state("current", before);
    let Some(change) = args.action.unwrap_or(BacklightAction::Status).change() else {
        println!("  offers: {}", offers(before.controls));
        return Ok(());
    };

    // The keyboard sets this mode itself from its backlight keys and
    // setBacklightConfig cannot write it back, so say so rather than let the
    // mode change look like a side effect of an unrelated change.
    if before.mode == BacklightMode::TemporaryManual && keeps_the_mode(change) {
        println!(
            "  note: the level came from the keyboard's backlight keys, a mode software cannot write back — it returns to automatic (ambient-light sensor)"
        );
    }

    let after = openlogi_hid::update_backlight(&route, change)
        .await
        .with_context(|| format!("set {change}"))?;
    print_state("read-back", after);
    println!("✓ {change} — persisted to the keyboard, survives reconnect and power cycle");
    Ok(())
}

/// Whether `change` writes the stored mode back as it was read.
fn keeps_the_mode(change: BacklightChange) -> bool {
    matches!(
        change,
        BacklightChange::Enabled(_) | BacklightChange::PowerSave(_)
    )
}

async fn read_state(route: &DeviceRoute) -> Result<BacklightState> {
    openlogi_hid::get_backlight(route)
        .await
        .context("read backlight state")
}

fn print_state(label: &str, state: BacklightState) {
    println!(
        "  {label}: enabled={} mode={} status={} level={}/{} battery-saving={}",
        state.enabled,
        mode_label(state.mode),
        status_label(state.status),
        state.current_level,
        max_backlight_level(state.nb_levels),
        state.power_save,
    );
}

/// The subcommands beyond `on` / `off` this keyboard accepts.
fn offers(controls: BacklightControls) -> String {
    let offered: Vec<&str> = [
        (controls.automatic, "auto"),
        (controls.level, "level"),
        (controls.power_save, "battery-saving"),
    ]
    .into_iter()
    .filter_map(|(offered, name)| offered.then_some(name))
    .collect();
    if offered.is_empty() {
        "on/off only".to_string()
    } else {
        format!("on/off, {}", offered.join(", "))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_writes_its_own_change() {
        assert_eq!(BacklightAction::Status.change(), None);
        assert_eq!(
            BacklightAction::Off.change(),
            Some(BacklightChange::Enabled(false))
        );
        assert_eq!(
            BacklightAction::Auto { set: Switch::Off }.change(),
            Some(BacklightChange::Automatic(false))
        );
        assert_eq!(
            BacklightAction::Level { level: 3 }.change(),
            Some(BacklightChange::Level(3))
        );
        assert_eq!(
            BacklightAction::BatterySaving { set: Switch::On }.change(),
            Some(BacklightChange::PowerSave(true))
        );
    }

    #[test]
    fn status_lists_only_what_the_keyboard_offers() {
        assert_eq!(offers(BacklightControls::default()), "on/off only");
        let level_only = BacklightControls {
            level: true,
            ..BacklightControls::default()
        };
        assert_eq!(offers(level_only), "on/off, level");
        let all = BacklightControls {
            automatic: true,
            level: true,
            power_save: true,
        };
        assert_eq!(offers(all), "on/off, auto, level, battery-saving");
    }

    #[test]
    fn only_a_change_that_keeps_the_mode_warns_about_the_backlight_keys() {
        assert!(keeps_the_mode(BacklightChange::Enabled(true)));
        assert!(keeps_the_mode(BacklightChange::PowerSave(false)));
        assert!(!keeps_the_mode(BacklightChange::Automatic(false)));
        assert!(!keeps_the_mode(BacklightChange::Level(2)));
    }
}
