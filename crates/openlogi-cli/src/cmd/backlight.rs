//! `openlogi backlight` — read and set the HID++ `0x1982` keyboard backlight.
//!
//! Unlike `diag`, this is persistent configuration: `setBacklightConfig` writes
//! to the keyboard's non-volatile memory, so every change here survives
//! reconnects, host switches, and power cycles with nothing re-applying it.

use std::io::{self, Write};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use openlogi_core::device::Capabilities;
use openlogi_core::hid::backlight::max_backlight_level;
use openlogi_core::hid::{
    BacklightChange, BacklightControls, BacklightDuration, BacklightMode, BacklightState,
    BacklightStatus, DeviceRoute,
};
use openlogi_ipc::client::ConnectError;
use openlogi_ipc::{AgentClient, AgentSnapshot, InventoryHealth};
use tarpc::context;

use crate::agent::{self, CallFailure};
use crate::cmd::api::Switch;

#[derive(Debug, Args)]
#[command(
    after_help = "Requires a running, compatible OpenLogi agent in the same OPENLOGI_PROFILE. No direct hardware fallback. Without --device, selects only one online device with measured backlight support. Ambiguous matches are refused; unknown support requires an explicit name."
)]
pub struct BacklightArgs {
    /// Select exactly one online device by a case-insensitive name substring.
    /// Refuses ambiguous matches instead of choosing the first keyboard.
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
    /// How long the keys stay lit after you stop typing, in seconds. A
    /// multiple of 5, from 5 through 7200 (two hours, the longest `0x1982`
    /// stores). Longer than a minute keeps the keys lit much longer, which
    /// costs battery life.
    Duration {
        /// Seconds, a multiple of 5 from 5 through 7200.
        #[arg(value_name = "SECONDS", value_parser = duration_seconds)]
        duration: BacklightDuration,
    },
}

/// Reject a duration that is not one the keyboard can store, so a typo never
/// reaches the write layer — the validated type makes it unrepresentable
/// before `update_backlight` is even called.
fn duration_seconds(raw: &str) -> Result<BacklightDuration, String> {
    raw.parse::<u16>()
        .ok()
        .and_then(|seconds| BacklightDuration::try_from(seconds).ok())
        .ok_or_else(duration_rejected)
}

fn duration_rejected() -> String {
    format!(
        "duration must be a multiple of {} seconds, from {} through {} (two hours)",
        BacklightDuration::UNIT_SECONDS,
        BacklightDuration::UNIT_SECONDS,
        BacklightDuration::MAX_SECONDS,
    )
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
            Self::Duration { duration } => Some(BacklightChange::Duration(duration)),
        }
    }
}

pub async fn run(args: BacklightArgs) -> Result<()> {
    let connection = agent::connect().await;
    execute(args, connection, &mut io::stdout()).await
}

/// Keep connection failures on the same no-fallback path as device operations.
/// The output sink and connection result also let tests exercise that boundary
/// without a socket, running agent, or hardware.
async fn execute(
    args: BacklightArgs,
    connection: Result<AgentClient, ConnectError>,
    out: &mut impl Write,
) -> Result<()> {
    let client = connection.context(
        "backlight requires a running, compatible OpenLogi agent; start the matching OpenLogi build with the same OPENLOGI_PROFILE (no direct hardware fallback)",
    )?;
    let snapshot = agent::snapshot(&client).await?;
    let (route, name) = select_device(&snapshot, args.device.as_deref())?;
    writeln!(out, "device: {name} ({route})")?;

    let before = agent::call(client.read_backlight(context::current(), route.clone()))
        .await
        .map_err(call_error)?
        .context("read backlight state")?;
    print_state(out, "current", before)?;
    let Some(change) = args.action.unwrap_or(BacklightAction::Status).change() else {
        writeln!(out, "  offers: {}", offers(before.controls))?;
        return Ok(());
    };

    // The keyboard sets this mode itself from its backlight keys and
    // setBacklightConfig cannot write it back, so say so rather than let the
    // mode change look like a side effect of an unrelated change.
    if before.mode == BacklightMode::TemporaryManual && keeps_the_mode(change) {
        writeln!(
            out,
            "  note: the level came from the keyboard's backlight keys, a mode software cannot write back — it returns to automatic (ambient-light sensor)"
        )?;
    }

    let after = agent::call(client.set_backlight(context::current(), route, change))
        .await
        .map_err(call_error)
        .context("backlight write outcome is unconfirmed; read status before deciding whether to try again")?
        .with_context(|| format!("set {change}"))?;
    print_state(out, "read-back", after)?;
    writeln!(
        out,
        "✓ {change} — persisted to the keyboard, survives reconnect and power cycle"
    )?;
    Ok(())
}

fn call_error(failure: CallFailure) -> anyhow::Error {
    match failure {
        CallFailure::TimedOut => anyhow!("agent backlight call timed out"),
        CallFailure::Disconnected => anyhow!("agent disconnected during backlight call"),
    }
}

/// Human-readable selection differs from the JSON API's exact opaque IDs.
/// Only the agent's HID++ inventory is eligible, never standalone raw-HID lights.
fn select_device(snapshot: &AgentSnapshot, query: Option<&str>) -> Result<(DeviceRoute, String)> {
    if snapshot.status.inventory != InventoryHealth::Ready {
        bail!("agent inventory is not ready; wait for OpenLogi to finish scanning and try again");
    }
    let devices: Vec<_> = snapshot
        .inventory
        .iter()
        .flat_map(|inventory| {
            inventory
                .paired
                .iter()
                .filter(|device| device.online)
                .map(|device| {
                    (
                        DeviceRoute::for_slot(inventory, device.slot),
                        device
                            .codename
                            .clone()
                            .unwrap_or_else(|| format!("Slot {}", device.slot)),
                        device.capabilities.as_ref(),
                    )
                })
        })
        .collect();
    let needle = query.map(str::to_lowercase);
    let matches: Vec<_> = devices
        .iter()
        .filter(|(_, name, caps)| {
            needle.as_ref().map_or_else(
                || caps.is_some_and(|caps: &Capabilities| caps.backlight),
                |needle| name.to_lowercase().contains(needle),
            )
        })
        .collect();
    let selected = if let [selected] = matches.as_slice() {
        *selected
    } else {
        let reason = if matches.is_empty() {
            "no matching online backlight device"
        } else {
            "ambiguous backlight device selection"
        };
        let list = devices
            .iter()
            .map(|(route, name, _)| {
                format!(
                    "  - {name} ({})",
                    route
                        .as_ref()
                        .map_or_else(|| "route unavailable".into(), ToString::to_string)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "{reason}; use --device <unique name substring>. Automatic selection requires measured backlight support.\nOnline HID++ devices:\n{list}"
        );
    };
    let (route, name, caps) = selected;
    let route = route
        .as_ref()
        .context("selected device has no resolvable agent route")?;
    // Distinct names cannot disambiguate identical direct VID/PID routes.
    if devices
        .iter()
        .filter(|(candidate, _, _)| candidate.as_ref() == Some(route))
        .count()
        != 1
    {
        bail!("ambiguous agent route for {name} ({route}); no operation was attempted");
    }
    if caps.is_some_and(|caps| !caps.backlight) {
        bail!("{name} does not advertise HID++ Backlight2 support");
    }
    Ok((route.clone(), name.clone()))
}

/// Whether `change` writes the stored mode back as it was read.
fn keeps_the_mode(change: BacklightChange) -> bool {
    matches!(
        change,
        BacklightChange::Enabled(_) | BacklightChange::PowerSave(_) | BacklightChange::Duration(_)
    )
}

fn print_state(out: &mut impl Write, label: &str, state: BacklightState) -> io::Result<()> {
    writeln!(
        out,
        "  {label}: enabled={} mode={} status={} level={}/{} battery-saving={} duration={}",
        state.enabled,
        mode_label(state.mode),
        status_label(state.status),
        state.current_level,
        max_backlight_level(state.nb_levels),
        state.power_save,
        duration_label(state)
    )
}

/// The timer this keyboard reports, or that it does not offer one.
fn duration_label(state: BacklightState) -> String {
    match state.duration {
        Some(timer) => format!("{}s", timer.seconds()),
        None => "unsupported".to_string(),
    }
}

/// The subcommands beyond `on` / `off` this keyboard accepts.
fn offers(controls: BacklightControls) -> String {
    let offered: Vec<&str> = [
        (BacklightControls::AUTOMATIC, "auto"),
        (BacklightControls::LEVEL, "level"),
        (BacklightControls::POWER_SAVE, "battery-saving"),
        (BacklightControls::DURATION, "duration"),
    ]
    .into_iter()
    .filter_map(|(offer, name)| controls.contains(offer).then_some(name))
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

    /// A test duration; the type guarantees every constructed value is legal.
    fn duration(seconds: u16) -> BacklightDuration {
        BacklightDuration::try_from(seconds).expect("a listed step is a valid duration")
    }

    #[test]
    fn the_parser_takes_only_what_the_keyboard_can_store() {
        assert_eq!(
            duration_seconds("15"),
            Ok(duration(15)),
            "the short step the panel shows"
        );
        assert_eq!(duration_seconds("7200"), Ok(duration(7_200)));
        assert_eq!(
            duration_seconds("7"),
            Err(duration_rejected()),
            "a value between the 5-second steps"
        );
        assert_eq!(duration_seconds("zero"), Err(duration_rejected()));
    }

    mod agent_routing;

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
        assert_eq!(
            BacklightAction::Duration {
                duration: duration(15)
            }
            .change(),
            Some(BacklightChange::Duration(duration(15)))
        );
    }

    #[test]
    fn status_lists_only_what_the_keyboard_offers() {
        assert_eq!(offers(BacklightControls::default()), "on/off only");
        assert_eq!(offers(BacklightControls::LEVEL), "on/off, level");
        assert_eq!(
            offers(
                BacklightControls::AUTOMATIC
                    | BacklightControls::LEVEL
                    | BacklightControls::POWER_SAVE
                    | BacklightControls::DURATION
            ),
            "on/off, auto, level, battery-saving, duration"
        );
    }

    #[test]
    fn only_a_change_that_keeps_the_mode_warns_about_the_backlight_keys() {
        assert!(keeps_the_mode(BacklightChange::Enabled(true)));
        assert!(keeps_the_mode(BacklightChange::PowerSave(false)));
        assert!(keeps_the_mode(BacklightChange::Duration(duration(10))));
        assert!(!keeps_the_mode(BacklightChange::Automatic(false)));
        assert!(!keeps_the_mode(BacklightChange::Level(2)));
    }
}
