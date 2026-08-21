//! `openlogi diag battery` — dump the device's raw battery report.
//!
//! Prints exactly what the firmware returns (unified `0x1004` fields, legacy
//! `0x1000` `discharge_level`/`next_level`/`status`, or `0x1001`
//! `voltage_mv`/`status`/`critical`). Run it once on battery and
//! once with the charger plugged in to see how the device reports while charging
//! — e.g. an MX2S returns `discharge_level=0` mid-charge, which is the device's
//! own limitation, not a bug in the read path.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct BatteryArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: BatteryArgs) -> Result<()> {
    // 0x1004 UnifiedBattery / 0x1000 BatteryStatus / 0x1001 BatteryVoltage — pick a device with any.
    let (route, name) = select_device(args.device.as_deref(), &[0x1000, 0x1001, 0x1004]).await?;
    println!("device: {name} ({route})");

    let line = openlogi_hid::read_battery_raw(&route)
        .await
        .context("read battery")?;
    println!("  {line}");
    Ok(())
}
