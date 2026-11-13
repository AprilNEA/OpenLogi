use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::{Dpi, OnboardDefaultDpi, OnboardMode, ProfileIndex};

use crate::cmd::diag::{exclude_agent, select_candidate};

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Run against the device whose name contains this string.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Write this DPI into flash as the profile's startup DPI: the speed the
    /// mouse runs at in onboard mode, before any host takes over. It stays
    /// across power cycles, and later DPI changes do not update it.
    #[arg(long, value_name = "DPI")]
    pub set_default_dpi: Option<u16>,

    /// The profile `--set-default-dpi` writes. Defaults to the active one.
    #[arg(long, value_name = "N", requires = "set_default_dpi")]
    pub profile: Option<u8>,
}

pub async fn run(args: OnboardArgs) -> Result<()> {
    // A flash write must not interleave with the agent's own requests, so
    // the agent stays excluded from before the device is chosen until the
    // write is read back. A plain read leaves the agent alone.
    let _agent_lock = match args.set_default_dpi {
        Some(_) => Some(exclude_agent().await?),
        None => None,
    };
    let chosen = select_candidate(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {} ({})", chosen.name, chosen.route);

    let state = openlogi_hid::get_onboard(&chosen.route)
        .await
        .context("read onboard memory")?;
    let mode = match state.mode {
        OnboardMode::Onboard => "onboard",
        OnboardMode::Host => "host",
    };
    println!("mode: {mode}");
    for profile in &state.profiles {
        let active = if state.active_profile == Some(profile.index) {
            "  [active]"
        } else {
            ""
        };
        let enabled = if profile.enabled { "" } else { "  (disabled)" };
        let name = profile.name.as_deref().unwrap_or("—");
        println!("  profile {}: {name}{enabled}{active}", profile.index);
    }

    let Some(dpi) = args.set_default_dpi else {
        return Ok(());
    };
    let unit = chosen.unit.context(
        "enumeration read no unit ID for this device, so the write cannot be bound to it",
    )?;
    let profile = match args.profile {
        Some(index) => ProfileIndex::new(index).context("profiles are numbered from 1")?,
        None => match state.active_profile {
            Some(profile) => profile,
            None => bail!("no active profile in {mode} mode; pass --profile"),
        },
    };
    let write = OnboardDefaultDpi {
        unit,
        profile,
        dpi: Dpi::new(dpi),
    };
    let change = openlogi_hid::set_onboard_default_dpi(&chosen.route, write)
        .await
        .context("write the onboard startup DPI")?;
    let before = change
        .before
        .map_or_else(|| "unset".to_owned(), |dpi| dpi.to_string());
    println!(
        "profile {} startup DPI: {before} → {}",
        change.profile, change.after
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use openlogi_core::single_instance::{self, InstanceError, Role};

    use super::{OnboardArgs, run};
    use crate::cmd::diag::exclude_agent;

    // Unix socket paths and the lock honour XDG_CONFIG_HOME, so a child
    // process with its own config home cannot meet a real agent; Windows
    // uses a fixed pipe name.
    #[cfg(unix)]
    #[test]
    fn a_startup_dpi_write_never_starts_beside_the_agent() {
        const TEST: &str =
            "cmd::diag::onboard::tests::a_startup_dpi_write_never_starts_beside_the_agent";
        const MODE: &str = "OPENLOGI_ONBOARD_EXCLUSION_TEST";
        let child = |mode: &str| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture"])
                .env(MODE, mode);
            command
        };
        match std::env::var(MODE).as_deref() {
            Ok("contender") => {
                // The acquisition the agent makes before any HID I/O.
                assert!(matches!(
                    single_instance::acquire(Role::Agent),
                    Err(InstanceError::AlreadyRunning { .. })
                ));
                return;
            }
            Ok("exercise") => {}
            _ => {
                let home = tempfile::tempdir().unwrap();
                let output = child("exercise")
                    .env("XDG_CONFIG_HOME", home.path())
                    .env_remove("XDG_RUNTIME_DIR")
                    .env(
                        openlogi_core::env::PROFILE,
                        openlogi_core::paths::Profile::Production.env_value(),
                    )
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
        }

        openlogi_core::worker::runtime().unwrap().block_on(async {
            {
                // An agent that holds its lock, even before its socket binds,
                // stops the command before it enumerates or opens anything.
                let _agent = single_instance::acquire(Role::Agent).unwrap();
                // A name no device has: should the exclusion regress, the
                // command fails at selection instead of reaching a real mouse.
                let error = run(OnboardArgs {
                    device: Some("no device has this name".into()),
                    set_default_dpi: Some(1200),
                    profile: None,
                })
                .await
                .expect_err("a write must not start while the agent holds its lock");
                assert!(matches!(
                    error.downcast_ref::<InstanceError>(),
                    Some(InstanceError::AlreadyRunning { .. })
                ));
            }
            {
                // Once admitted, the command keeps the agent from starting.
                let _admitted = exclude_agent().await.unwrap();
                let output = child("contender").output().unwrap();
                assert!(
                    output.status.success(),
                    "the agent must stay excluded: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            drop(single_instance::acquire(Role::Agent).unwrap());
        });
    }
}
