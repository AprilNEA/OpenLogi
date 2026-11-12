use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::{Dpi, OnboardMode, ProfileIndex};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Run against the device whose name contains this string.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Write this DPI into flash as the profile's startup DPI: the speed the
    /// mouse runs at in onboard mode, before any host takes over.
    #[arg(long, value_name = "DPI")]
    pub set_default_dpi: Option<u16>,

    /// The profile `--set-default-dpi` writes. Defaults to the active one.
    #[arg(long, value_name = "N", requires = "set_default_dpi")]
    pub profile: Option<u8>,
}

pub async fn run(args: OnboardArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let state = openlogi_hid::get_onboard(&route)
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
    let profile = match args.profile {
        Some(index) => ProfileIndex::new(index).context("profiles are numbered from 1")?,
        None => match state.active_profile {
            Some(profile) => profile,
            None => bail!("no active profile in {mode} mode; pass --profile"),
        },
    };
    let change = openlogi_hid::set_onboard_default_dpi(&route, profile, Dpi::new(dpi))
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
