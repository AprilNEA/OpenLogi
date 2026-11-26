//! `openlogi haptic` — play HID++ `0x19b0` haptic feedback on a mouse through
//! the running agent, for scripts and other apps.

use std::io::{self, Write};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};
use openlogi_core::hid::{HapticPulse, WriteError};
use openlogi_ipc::AgentClient;
use openlogi_ipc::client::ConnectError;
use tarpc::context;

use crate::agent::{self, CallFailure};

#[derive(Debug, Args)]
#[command(
    after_help = "Requires a running, compatible OpenLogi agent in the same OPENLOGI_PROFILE. No direct hardware fallback. Without --device, selects only one online device with measured haptic feedback (HID++ 0x19b0). Ambiguous matches are refused."
)]
pub struct HapticArgs {
    /// Select exactly one online device by a case-insensitive name substring.
    #[arg(long, value_name = "NAME", global = true)]
    pub device: Option<String>,

    #[command(subcommand)]
    pub action: HapticAction,
}

#[derive(Debug, Subcommand)]
pub enum HapticAction {
    /// Play one haptic pulse now.
    Play {
        #[arg(value_enum, default_value_t = Waveform::Tick)]
        waveform: Waveform,
    },
}

/// The waveforms a script can ask for, by what they feel like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Waveform {
    /// A light tick, as when the Actions Ring highlights another slot.
    Tick,
    /// A firmer confirmation pulse, as when an Actions Ring action runs.
    Confirm,
}

impl Waveform {
    pub(crate) fn pulse(self) -> HapticPulse {
        match self {
            Self::Tick => HapticPulse::SubtleCollision,
            Self::Confirm => HapticPulse::DampStateChange,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Tick => "tick",
            Self::Confirm => "confirm",
        }
    }
}

/// What automatic selection looks for, and what a refusal names.
const HAPTICS: agent::Feature = agent::Feature {
    noun: "haptic",
    measured: |caps| caps.haptic_feedback,
    missing: "HID++ haptic feedback (0x19b0)",
};

pub async fn run(args: HapticArgs) -> Result<()> {
    let connection = agent::connect().await;
    execute(args, connection, &mut io::stdout()).await
}

/// The connection result and output sink are parameters so tests reach the
/// whole path without a socket, an agent, or hardware.
async fn execute(
    args: HapticArgs,
    connection: Result<AgentClient, ConnectError>,
    out: &mut impl Write,
) -> Result<()> {
    let HapticAction::Play { waveform } = args.action;
    let client = connection.context(
        "haptic requires a running, compatible OpenLogi agent; start the matching OpenLogi build with the same OPENLOGI_PROFILE (no direct hardware fallback)",
    )?;
    let snapshot = agent::snapshot(&client).await?;
    let (route, name) = agent::select_device(&snapshot, args.device.as_deref(), &HAPTICS)?;
    agent::call(client.play_haptic(context::current(), route.clone(), waveform.pulse()))
        .await
        .map_err(|failure| match failure {
            CallFailure::TimedOut => anyhow!("agent haptic call timed out"),
            CallFailure::Disconnected => anyhow!("agent disconnected during haptic call"),
        })?
        .map_err(|error| match error {
            WriteError::WriteSuperseded { .. } => {
                anyhow!("a newer haptic replaced this one before it played")
            }
            error => anyhow!(error),
        })
        .with_context(|| format!("play {} on {name}", waveform.name()))?;
    writeln!(out, "played {} on {name} ({route})", waveform.name())?;
    Ok(())
}

#[cfg(test)]
mod tests;
