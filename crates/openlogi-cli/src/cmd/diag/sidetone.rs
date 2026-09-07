//! `openlogi diag sidetone [LEVEL]` — read or set headset audio sidetone (feature 0x0604).
//!
//! Controls the microphone loopback / sidetone level in percentage (0..=100).
//! Without an argument, reads and prints the current sidetone level.

use anyhow::{Context, Result, anyhow};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct SidetoneArgs {
    /// Desired sidetone level in percentage (0..=100).
    pub level: Option<u8>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: SidetoneArgs) -> Result<()> {
    if let Some(level) = args.level
        && level > 100
    {
        return Err(anyhow!("sidetone level must be between 0 and 100 percent, got {level}"));
    }

    // 0x0604 HeadsetAudioSidetone / 0x8300 Sidetone
    let (route, name) = select_device(args.device.as_deref(), &[0x0604, 0x8300]).await?;
    println!("device: {name} ({route})");

    let current = openlogi_hid::get_sidetone_level(&route)
        .await
        .context("read sidetone level")?;
    println!("  current sidetone level: {current}%");

    if let Some(target) = args.level {
        openlogi_hid::set_sidetone_level(&route, target)
            .await
            .context("write sidetone level")?;
        let verified = openlogi_hid::get_sidetone_level(&route)
            .await
            .context("verify sidetone level")?;
        println!("  updated sidetone level: {verified}%");
    }

    Ok(())
}
