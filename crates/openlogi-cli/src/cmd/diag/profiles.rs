//! `openlogi diag profiles` — onboard-profiles (HID++ `0x8100`) round-trip.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::{DeviceRoute, OnboardProfilesInfo, ProfilesMode};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct ProfilesArgs {
    /// Only read and print the onboard-profiles state; skip the mode and
    /// active-profile write round-trips.
    #[arg(long, conflicts_with = "leave_onboard")]
    pub read_only: bool,

    /// Leave the device in onboard mode (skip the final restore to the
    /// original mode). Useful for verifying the agent's host-mode reapply
    /// on reconnect, or the onboard behaviour visually.
    #[arg(long)]
    pub leave_onboard: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: ProfilesArgs) -> Result<()> {
    // 0x8100 = OnboardProfiles — auto-skip devices without profile memory.
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::get_onboard_profiles(&route)
        .await
        .context("read onboard-profiles state")?;
    print_info(&info);

    if args.read_only {
        return Ok(());
    }

    // Enter onboard mode first: the firmware rejects setCurrentProfile with
    // InvalidArgument while in host mode (verified on a G502 X LIGHTSPEED),
    // so the profile round-trip must run inside the onboard-mode window. This
    // also exercises the mode write path in both directions.
    if info.mode != ProfilesMode::Onboard {
        println!("  entering mode: {:?} -> Onboard", info.mode);
        let read_back = openlogi_hid::set_profiles_mode(&route, ProfilesMode::Onboard)
            .await
            .context("write onboard mode")?;
        if read_back != ProfilesMode::Onboard {
            anyhow::bail!(
                "onboard mode write not applied: requested Onboard, device reports {read_back:?}"
            );
        }
    }

    let round_trip = profile_round_trip(&route, &info).await;

    if args.leave_onboard {
        round_trip?;
        println!("✓ onboard-profiles diag OK (device left in Onboard mode)");
        return Ok(());
    }

    // Restore the original mode last, so the device leaves the diag exactly as
    // it entered (host-mode users get host mode back) — including when the
    // round-trip failed, which is precisely when the device is left changed.
    let restore = restore_mode(&route, info.mode).await;
    round_trip?;
    restore?;

    println!("✓ onboard-profiles diag OK");
    Ok(())
}

/// Activate an enabled profile and put the original one back.
///
/// Prefers a sector that is not already active; a single-profile device
/// re-writes the same sector, which still exercises the write path.
async fn profile_round_trip(route: &DeviceRoute, info: &OnboardProfilesInfo) -> Result<()> {
    let target = info
        .directory
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.sector)
        .find(|&s| s != info.active_profile)
        .or_else(|| info.directory.iter().find(|e| e.enabled).map(|e| e.sector));
    match target {
        None => {
            println!("  no enabled profiles in the directory — profile round-trip skipped");
        }
        Some(target) => {
            if target == info.active_profile {
                println!(
                    "  only one enabled profile — set/restore exercised against sector {target:#06x}"
                );
            }
            println!("  activating profile sector {target:#06x}");
            let read_back = openlogi_hid::set_active_profile(route, target)
                .await
                .context("write active profile")?;
            if read_back != target {
                anyhow::bail!(
                    "active-profile write not applied: requested {target:#06x}, device reports {read_back:#06x}"
                );
            }
            // Restore, unless the device had never activated a profile
            // (0x0000 is not a writable target).
            if info.active_profile == 0 {
                println!("  original active profile was 0x0000 (none) — restore skipped");
            } else if info.active_profile != target {
                println!("  restoring profile sector {:#06x}", info.active_profile);
                let restored = openlogi_hid::set_active_profile(route, info.active_profile)
                    .await
                    .context("restore active profile")?;
                if restored != info.active_profile {
                    anyhow::bail!(
                        "active-profile restore not applied: requested {:#06x}, device reports {restored:#06x}",
                        info.active_profile
                    );
                }
            }
            println!("  ✓ profile round-trip OK");
        }
    }
    Ok(())
}

/// Put the device back into `mode`. A no-op when the diag never left it.
async fn restore_mode(route: &DeviceRoute, mode: ProfilesMode) -> Result<()> {
    if mode == ProfilesMode::Onboard {
        return Ok(());
    }
    println!("  restoring mode: {mode:?}");
    let restored = openlogi_hid::set_profiles_mode(route, mode)
        .await
        .context("restore onboard mode")?;
    if restored != mode {
        anyhow::bail!(
            "onboard mode restore not applied: requested {mode:?}, device reports {restored:?}"
        );
    }
    println!("  ✓ mode round-trip OK");
    Ok(())
}

/// Print the description, mode, active profile, and directory table.
fn print_info(info: &OnboardProfilesInfo) {
    println!(
        "  memory: {} user + {} ROM profiles, {} buttons, {} sectors x {} bytes",
        info.profile_count,
        info.profile_count_oob,
        info.button_count,
        info.sector_count,
        info.sector_size
    );
    println!(
        "  formats: memory_model={} profile={} macro={}",
        info.memory_model_id, info.profile_format_id, info.macro_format_id
    );
    println!("  mode: {:?}", info.mode);
    match info.active_profile {
        0 => println!("  active profile: none reported (0x0000)"),
        sector if openlogi_hid::is_rom_sector(sector) => {
            println!("  active profile: {sector:#06x} (ROM)");
        }
        sector => println!("  active profile: {sector:#06x}"),
    }
    if info.directory.is_empty() {
        println!("  directory: empty (erased flash or no profiles written)");
    } else {
        println!("  directory:");
        for entry in &info.directory {
            println!(
                "    sector {:#06x}  {}{}",
                entry.sector,
                if entry.enabled { "enabled" } else { "disabled" },
                if entry.is_rom() { " (ROM)" } else { "" }
            );
        }
    }
}
