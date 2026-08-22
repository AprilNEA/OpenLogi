//! `openlogi diag profiles` — onboard-profiles (HID++ `0x8100`) round-trip.

use anyhow::{Context, Result, anyhow};
use clap::Args;
use openlogi_hid::{DeviceRoute, OnboardProfilesInfo, ProfilesMode, is_rom_sector};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct ProfilesArgs {
    /// Only read and print the onboard-profile state; skip all writes.
    #[arg(long, conflicts_with = "leave_onboard")]
    pub read_only: bool,

    /// Leave the device in onboard mode after the diagnostic. Useful for
    /// visually checking an onboard profile or the agent's reconnect reapply.
    #[arg(long)]
    pub leave_onboard: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: ProfilesArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::get_onboard_profiles(&route)
        .await
        .context("read onboard-profile state")?;
    print_info(&info);
    if args.read_only {
        return Ok(());
    }

    // Firmware rejects setCurrentProfile in host mode, so the profile test
    // runs inside an onboard-mode window. Capture the whole operation as a
    // result so a failure still reaches the mode-restoration path below.
    let operation = match enter_onboard(&route, info.mode).await {
        Ok(()) => profile_round_trip(&route, &info).await,
        Err(error) => Err(error),
    };

    if args.leave_onboard {
        operation?;
        println!("✓ onboard-profile diagnostic OK (device left in onboard mode)");
        return Ok(());
    }

    let restore = restore_mode(&route, info.mode).await;
    finish_with_restore(operation, restore)?;
    println!("✓ onboard-profile diagnostic OK");
    Ok(())
}

async fn enter_onboard(route: &DeviceRoute, original: ProfilesMode) -> Result<()> {
    if original == ProfilesMode::Onboard {
        return Ok(());
    }
    println!("  entering mode: {original:?} -> Onboard");
    let read_back = openlogi_hid::set_profiles_mode(route, ProfilesMode::Onboard)
        .await
        .context("write onboard mode")?;
    if read_back != ProfilesMode::Onboard {
        anyhow::bail!(
            "onboard mode write not applied: requested Onboard, device reports {read_back:?}"
        );
    }
    Ok(())
}

/// Activate an enabled user profile and restore the original user profile.
async fn profile_round_trip(route: &DeviceRoute, info: &OnboardProfilesInfo) -> Result<()> {
    let Some(target) = round_trip_target(info) else {
        if info.mode == ProfilesMode::Onboard
            && (info.active_profile == 0 || is_rom_sector(info.active_profile))
        {
            println!(
                "  active onboard profile is not a restorable user sector — profile round-trip skipped"
            );
        } else {
            println!("  no enabled user profiles in the directory — profile round-trip skipped");
        }
        return Ok(());
    };

    if target == info.active_profile {
        println!("  only one enabled user profile — exercising sector {target:#06x} in place");
    }
    println!("  activating profile sector {target:#06x}");
    let write = openlogi_hid::set_active_profile(route, target)
        .await
        .context("write active profile")
        .and_then(|read_back| {
            if read_back == target {
                Ok(())
            } else {
                anyhow::bail!(
                    "active-profile write not applied: requested {target:#06x}, device reports {read_back:#06x}"
                )
            }
        });

    // A failed write may still have reached the device before its confirming
    // read failed. Restore whenever the original is a distinct user sector.
    let restore = restore_profile(route, info.active_profile, target).await;
    finish_with_restore(write, restore)?;
    println!("  ✓ profile round-trip OK");
    Ok(())
}

fn round_trip_target(info: &OnboardProfilesInfo) -> Option<u16> {
    if info.mode == ProfilesMode::Onboard
        && (info.active_profile == 0 || is_rom_sector(info.active_profile))
    {
        return None;
    }
    let mut enabled_users = info
        .directory
        .iter()
        .filter(|entry| entry.enabled && !entry.is_rom())
        .map(|entry| entry.sector);
    enabled_users
        .find(|&sector| sector != info.active_profile)
        .or_else(|| {
            info.directory
                .iter()
                .find(|entry| entry.enabled && !entry.is_rom())
                .map(|entry| entry.sector)
        })
}

async fn restore_profile(route: &DeviceRoute, original: u16, target: u16) -> Result<()> {
    if original == 0 || original == target || is_rom_sector(original) {
        return Ok(());
    }
    println!("  restoring profile sector {original:#06x}");
    let restored = openlogi_hid::set_active_profile(route, original)
        .await
        .context("restore active profile")?;
    if restored != original {
        anyhow::bail!(
            "active-profile restore not applied: requested {original:#06x}, device reports {restored:#06x}"
        );
    }
    Ok(())
}

async fn restore_mode(route: &DeviceRoute, original: ProfilesMode) -> Result<()> {
    if original == ProfilesMode::Onboard {
        return Ok(());
    }
    println!("  restoring mode: {original:?}");
    let restored = openlogi_hid::set_profiles_mode(route, original)
        .await
        .context("restore onboard mode")?;
    if restored != original {
        anyhow::bail!(
            "onboard mode restore not applied: requested {original:?}, device reports {restored:?}"
        );
    }
    println!("  ✓ mode round-trip OK");
    Ok(())
}

fn finish_with_restore(operation: Result<()>, restore: Result<()>) -> Result<()> {
    match (operation, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(restore)) => Err(anyhow!(
            "{operation:#}; additionally failed to restore device state: {restore:#}"
        )),
    }
}

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
        sector if is_rom_sector(sector) => println!("  active profile: {sector:#06x} (ROM)"),
        sector => println!("  active profile: {sector:#06x}"),
    }
    if info.directory.is_empty() {
        println!("  directory: empty (erased flash or no profiles written)");
        return;
    }
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

#[cfg(test)]
mod tests {
    use openlogi_hid::{OnboardProfilesInfo, ProfileEntry, ProfilesMode};

    use super::{finish_with_restore, round_trip_target};

    fn info(mode: ProfilesMode, active_profile: u16) -> OnboardProfilesInfo {
        OnboardProfilesInfo {
            profile_count: 2,
            profile_count_oob: 1,
            button_count: 11,
            sector_count: 4,
            sector_size: 254,
            memory_model_id: 1,
            profile_format_id: 1,
            macro_format_id: 1,
            mode,
            active_profile,
            directory: vec![
                ProfileEntry {
                    sector: 1,
                    enabled: true,
                },
                ProfileEntry {
                    sector: 2,
                    enabled: true,
                },
                ProfileEntry {
                    sector: 0x0101,
                    enabled: true,
                },
            ],
        }
    }

    #[test]
    fn target_prefers_an_alternate_enabled_user_sector() {
        assert_eq!(round_trip_target(&info(ProfilesMode::Onboard, 1)), Some(2));
    }

    #[test]
    fn target_never_selects_or_replaces_an_active_rom_sector() {
        assert_eq!(
            round_trip_target(&info(ProfilesMode::Onboard, 0x0101)),
            None
        );
    }

    #[test]
    fn host_mode_with_no_active_profile_can_exercise_a_user_sector() {
        assert_eq!(round_trip_target(&info(ProfilesMode::Host, 0)), Some(1));
    }

    #[test]
    fn operation_error_is_not_hidden_when_restoration_also_fails() {
        let Err(error) = finish_with_restore(
            Err(anyhow::anyhow!("write")),
            Err(anyhow::anyhow!("restore")),
        ) else {
            panic!("both failures must be reported");
        };
        let error = error.to_string();
        assert!(error.contains("write"));
        assert!(error.contains("restore"));
    }
}
