//! `openlogi haptic` — read and set the HID++ `0x19b0` haptic feedback.
//!
//! Unlike `backlight`, this is volatile: `setConfig` writes to device RAM, so
//! the intensity is lost on a power cycle and something has to re-apply it.
//! `play` is fire-and-forget and stores nothing.

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use openlogi_hid::{DeviceRoute, HapticIntensity, HapticWaveform};

use crate::cmd::diag::select_device;

/// HID++ `HapticFeedback` — the haptic motor behind the MX Master 4's
/// Actions Ring panel.
const HAPTIC_FEATURE: u16 = 0x19b0;

#[derive(Debug, Args)]
pub struct HapticArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME", global = true)]
    pub device: Option<String>,

    #[command(subcommand)]
    pub action: Option<HapticAction>,
}

#[derive(Debug, Subcommand)]
pub enum HapticAction {
    /// Show the current haptic configuration and the waveforms the device
    /// advertises (the default with no subcommand).
    Status,
    /// Set the haptic intensity, 0-100. Zero silences haptics.
    Level {
        /// Intensity percentage.
        value: u8,
    },
    /// Play one waveform immediately. Stores nothing.
    Play {
        /// Waveform to play.
        #[arg(value_enum)]
        waveform: Waveform,
    },
}

/// The waveforms accepted by `playWaveform`, as CLI values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Waveform {
    /// A crisp state-change pulse.
    SharpStateChange,
    /// A damped state-change pulse; the ring plays this on activation.
    DampStateChange,
    /// A crisp boundary pulse.
    SharpCollision,
    /// A damped boundary pulse.
    DampCollision,
    /// The lightest boundary pulse; the ring plays this on hover.
    SubtleCollision,
    /// A positive two-tone alert.
    HappyAlert,
    /// A negative two-tone alert.
    AngryAlert,
    /// A completion flourish.
    Completed,
    /// A square-wave buzz.
    Square,
    /// A rolling wave.
    Wave,
    /// A burst pattern.
    Firework,
    /// An agitated pattern.
    Mad,
    /// A double-tap knock.
    Knock,
    /// A short melodic pattern.
    Jingle,
    /// A repeating ring.
    Ringing,
    /// The faintest boundary pulse.
    WhisperCollision,
}

impl From<Waveform> for HapticWaveform {
    fn from(value: Waveform) -> Self {
        match value {
            Waveform::SharpStateChange => Self::SharpStateChange,
            Waveform::DampStateChange => Self::DampStateChange,
            Waveform::SharpCollision => Self::SharpCollision,
            Waveform::DampCollision => Self::DampCollision,
            Waveform::SubtleCollision => Self::SubtleCollision,
            Waveform::HappyAlert => Self::HappyAlert,
            Waveform::AngryAlert => Self::AngryAlert,
            Waveform::Completed => Self::Completed,
            Waveform::Square => Self::Square,
            Waveform::Wave => Self::Wave,
            Waveform::Firework => Self::Firework,
            Waveform::Mad => Self::Mad,
            Waveform::Knock => Self::Knock,
            Waveform::Jingle => Self::Jingle,
            Waveform::Ringing => Self::Ringing,
            Waveform::WhisperCollision => Self::WhisperCollision,
        }
    }
}

pub async fn run(args: HapticArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[HAPTIC_FEATURE]).await?;
    println!("device: {name} ({route})");

    match args.action.unwrap_or(HapticAction::Status) {
        HapticAction::Status => print_status(&route).await?,
        HapticAction::Level { value } => set_level(&route, value).await?,
        HapticAction::Play { waveform } => {
            openlogi_hid::play_haptic(&route, waveform.into())
                .await
                .context("play waveform")?;
            println!("✓ played {waveform:?}");
        }
    }

    Ok(())
}

async fn print_status(route: &DeviceRoute) -> Result<()> {
    let (config, caps) = openlogi_hid::get_haptic_state(route)
        .await
        .context("read haptic state")?;
    println!(
        "  enabled: {}\n  intensity: {}%\n  levels: {} in steps of {}%",
        config.enabled,
        config.intensity.get(),
        config.level_count,
        config.level_step,
    );
    println!("  supported waveforms: {:?}", caps.waveforms);
    Ok(())
}

async fn set_level(route: &DeviceRoute, value: u8) -> Result<()> {
    let intensity = HapticIntensity::new(value)
        .with_context(|| format!("intensity must be 0-100, got {value}"))?;

    let after = openlogi_hid::set_haptic_intensity(route, intensity)
        .await
        .context("write haptic intensity")?;

    if after.intensity == intensity {
        println!("✓ intensity {value}% — volatile, lost on a power cycle");
    } else {
        println!(
            "haptic write not applied: requested {value}%, device reports {}%",
            after.intensity.get()
        );
    }
    Ok(())
}
