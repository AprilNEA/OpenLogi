//! `openlogi diag onboard` — read a gaming mouse's onboard memory
//! (`0x8100 OnboardProfiles`): its mode, the active profile decoded, and
//! the raw directory and profile sectors; optionally switch its mode.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use openlogi_core::hid::{MemoryMode, OnboardLighting, OnboardProfileChange, ReportRate};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Switch the mouse to this mode before reading. The mouse returns to
    /// onboard mode by itself when it power-cycles.
    #[arg(long, value_enum)]
    pub set_mode: Option<ModeArg>,

    /// Print the directory and active profile sectors as hex.
    #[arg(long)]
    pub raw: bool,

    /// Write the active profile with its report rate changed, read it back,
    /// then write the original back and check the memory matches it again.
    #[arg(long)]
    pub round_trip: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

/// A mode on the command line.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ModeArg {
    /// The mouse runs its stored profile.
    Onboard,
    /// Host software drives the mouse.
    Software,
}

impl From<ModeArg> for MemoryMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Onboard => Self::Onboard,
            ModeArg::Software => Self::Software,
        }
    }
}

pub async fn run(args: OnboardArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    if let Some(mode) = args.set_mode {
        openlogi_hid::set_memory_mode(&route, mode.into())
            .await
            .context("switch onboard mode")?;
        println!("  mode switched to {:?}", MemoryMode::from(mode));
    }

    let dump = openlogi_hid::dump_onboard_memory(&route)
        .await
        .context("read onboard memory")?;
    let info = dump.info;
    println!(
        "  layout: memory model {} / profile format {} / macro format {}",
        info.memory_model_id, info.profile_format_id, info.macro_format_id
    );
    println!(
        "  {} user profiles, {} factory, {} buttons, {} sectors of {} bytes",
        info.profile_count,
        info.rom_profile_count,
        info.button_count,
        info.sector_count,
        info.sector_size
    );
    println!("  mode: {:?}", dump.mode);
    println!("  active profile sector: {:#06x}", dump.active.raw());

    match openlogi_hid::read_onboard_memory(&route).await {
        Ok(memory) => match memory.profile {
            Some(profile) => {
                println!(
                    "  profile {}{}{}",
                    profile.number,
                    profile
                        .name
                        .map(|n| format!(" \"{n}\""))
                        .unwrap_or_default(),
                    if profile.factory { " (factory)" } else { "" }
                );
                match profile.report_rate {
                    Some(rate) => println!("    report rate: {rate} Hz"),
                    None => println!("    report rate: not set"),
                }
                for (slot, dpi) in profile.dpi_slots.iter().enumerate() {
                    let marker = if usize::from(profile.default_dpi_slot) == slot {
                        " (default)"
                    } else {
                        ""
                    };
                    match dpi {
                        Some(dpi) => println!("    DPI slot {}: {dpi}{marker}", slot + 1),
                        None => println!("    DPI slot {}: unused{marker}", slot + 1),
                    }
                }
                for (zone, lighting) in profile.lighting.iter().enumerate() {
                    println!("    LED zone {}: {}", zone + 1, describe(*lighting));
                }
            }
            None => println!("  profile: layout not decoded"),
        },
        Err(e) => println!("  profile decode failed: {e:#}"),
    }

    if args.round_trip {
        round_trip(&route, &dump.profile).await?;
    }

    if args.raw {
        println!("  directory:\n{}", hex(&dump.directory));
        println!("  active profile:\n{}", hex(&dump.profile));
    }
    Ok(())
}

/// Change the report rate of the active profile, check the mouse keeps it,
/// then restore the profile and check its bytes match `original`.
async fn round_trip(route: &openlogi_hid::DeviceRoute, original: &[u8]) -> Result<()> {
    let memory = openlogi_hid::read_onboard_memory(route)
        .await
        .context("read onboard memory")?;
    let profile = memory
        .profile
        .context("the mouse's profile layout is not decoded")?;
    // The test's restore writes only the modeled settings: on a factory
    // profile the first write would create and select a user profile that
    // the restore cannot take back.
    anyhow::ensure!(
        !profile.factory,
        "the mouse runs factory profile {}: save a profile to it first, then run the round trip",
        profile.number
    );
    let rate = profile
        .report_rate
        .context("the profile stores no report rate")?;
    let restore = OnboardProfileChange {
        profile: profile.number,
        dpi_slots: profile.dpi_slots.clone(),
        default_dpi_slot: profile.default_dpi_slot,
        report_rate: rate,
        lighting: profile.lighting.clone(),
    };
    let other = if u16::from(rate) == 500 { 1000 } else { 500 };
    let change = OnboardProfileChange {
        report_rate: ReportRate::new(other),
        ..restore.clone()
    };
    println!("  round trip: report rate {rate} Hz → {other} Hz");
    // A failed write may still have saved the changed sector before a later
    // step failed: restore whatever the outcome, and keep both errors.
    let tested = openlogi_hid::write_onboard_profile(route, change)
        .await
        .context("write the changed profile")
        .and_then(|written| {
            let kept = written.profile.and_then(|p| p.report_rate);
            anyhow::ensure!(
                kept == Some(ReportRate::new(other)),
                "the mouse reads back {kept:?} after writing {other} Hz"
            );
            Ok(())
        });
    if tested.is_ok() {
        println!("  ✓ the mouse kept {other} Hz");
    }
    let restored = openlogi_hid::write_onboard_profile(route, restore)
        .await
        .context("restore the profile");
    match (tested, restored) {
        (Err(test), Err(restore)) => {
            anyhow::bail!("{test:#}; restoring the profile failed too: {restore:#}")
        }
        (Err(test), Ok(_)) => return Err(test.context("the profile was restored")),
        (Ok(()), restored) => {
            restored?;
        }
    }
    let after = openlogi_hid::dump_onboard_memory(route)
        .await
        .context("re-read onboard memory")?;
    anyhow::ensure!(
        after.profile == original,
        "the restored profile differs from the one read before the round trip"
    );
    println!("  ✓ restored: the profile matches the original byte for byte");
    Ok(())
}

fn describe(lighting: OnboardLighting) -> String {
    match lighting {
        OnboardLighting::Off => "off".to_owned(),
        OnboardLighting::Fixed(color) => format!("fixed #{color}"),
        OnboardLighting::Cycle {
            period_ms,
            brightness,
        } => format!("cycle {period_ms} ms at {brightness}%"),
        OnboardLighting::Breathing {
            color,
            period_ms,
            brightness,
        } => format!("breathing #{color} {period_ms} ms at {brightness}%"),
        OnboardLighting::Unsupported => "an effect OpenLogi does not edit".to_owned(),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "    {:04x}:", row * 16);
        for byte in chunk {
            let _ = write!(out, " {byte:02x}");
        }
        out.push('\n');
    }
    out
}
