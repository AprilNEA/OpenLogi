//! `openlogi diag sidetone` — read or set a headset's HID++ 0x8300 sidetone.

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::SidetoneLevel;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct SidetoneArgs {
    /// Level to write, in percent (0 is off, 100 the loudest). The headset
    /// stores it; nothing is written to config.toml.
    #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
    pub level: Option<u8>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: SidetoneArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8300]).await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_sidetone(&route)
        .await
        .context("read sidetone")?;
    println!("  current sidetone: {before}%");

    let Some(raw) = args.level else {
        return Ok(());
    };
    let requested = SidetoneLevel::try_new(raw).context("sidetone level")?;
    let applied = openlogi_hid::set_sidetone(&route, requested)
        .await
        .context("set sidetone")?;
    println!("  confirmed: {applied}%");
    if applied != requested {
        bail!("sidetone write not applied: requested {requested}%, device confirms {applied}%");
    }
    println!("✓ sidetone set to {applied}%");
    Ok(())
}
