//! `openlogi diag battery` — dump the device's raw battery report.
//!
//! Prints exactly what the firmware returns (unified `0x1004` fields, legacy
//! `0x1000` `discharge_level`/`next_level`/`status`, the `0x1001` measured
//! voltage and charging flags, or the `0x1F20` ADC voltage and charging state).
//! Run it once on battery and
//! once with the charger plugged in to see how the device reports while charging
//! — e.g. an MX2S returns `discharge_level=0` mid-charge, which is the device's
//! own limitation, not a bug in the read path.

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::{RAW_BATTERY_FEATURES, WriteError};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct BatteryArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: BatteryArgs) -> Result<()> {
    // Pick a device with any battery feature the read below understands.
    let (route, name) = select_device(args.device.as_deref(), &RAW_BATTERY_FEATURES).await?;
    println!("device: {name} ({route})");

    let line = match openlogi_hid::read_battery_raw(&route).await {
        Ok(line) => line,
        // The read reports its preferred feature; say instead that it found
        // none, so a device with some other battery feature isn't misread as
        // lacking `0x1004` specifically.
        Err(WriteError::FeatureUnsupported { .. }) => {
            let features: Vec<_> = RAW_BATTERY_FEATURES
                .iter()
                .map(|id| format!("{id:#06x}"))
                .collect();
            bail!(
                "read battery: device exposes none of the battery features this command reads ({})",
                features.join(", ")
            );
        }
        Err(e) => return Err(e).context("read battery"),
    };
    println!("  {line}");
    Ok(())
}
