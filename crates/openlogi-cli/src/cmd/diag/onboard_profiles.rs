//! `openlogi diag onboard-profiles` — dump the raw HID++ `0x8100
//! OnboardProfiles` info payload.
//!
//! Read-only. G-series gaming mice (G502 X, G502 X LIGHTSPEED, ...) expose no
//! `ReprogControls` (`0x1b00`–`0x1b04`) at all, so the desktop app's Buttons
//! panel never appears for them; they use `0x8100` for on-device profile and
//! button storage instead. This command exists to capture that feature's raw
//! payload from real hardware before its byte layout is decoded — see
//! `hidpp::feature::onboard_profiles` for why no fields are parsed yet.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct OnboardProfilesArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: OnboardProfilesArgs) -> Result<()> {
    // 0x8100 = OnboardProfiles.
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let payload = openlogi_hid::dump_onboard_profiles_info(&route)
        .await
        .context("dump HID++ 0x8100 onboard-profiles info")?;

    print!("  raw getInfo payload (function 0, undecoded):");
    for byte in payload {
        print!(" {byte:02x}");
    }
    println!();
    Ok(())
}
