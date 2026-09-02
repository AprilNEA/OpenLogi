//! `openlogi diag crown` — read the HID++ `0x4600 Crown` capabilities and
//! current mode on the Craft's rotary crown.
//!
//! Read-only: this is the M2 smoke test for `docs` roadmap purposes — it
//! confirms `CrownFeature::get_info`/`get_mode` decode real firmware
//! payloads, since `crown.rs` has so far only been exercised against fixture
//! bytes in its unit tests.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::{CrownInfo, CrownMode};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct CrownArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: CrownArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x4600]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::get_crown_info(&route)
        .await
        .context("read Crown info")?;
    print_info(info);

    let mode = openlogi_hid::get_crown_mode(&route)
        .await
        .context("read Crown mode")?;
    print_mode(mode);

    Ok(())
}

fn print_info(info: CrownInfo) {
    println!(
        "  info: controls={:?} sensors={:?} slots={} ratchets={}",
        info.controls, info.sensors, info.slots, info.ratchets
    );
}

fn print_mode(mode: CrownMode) {
    println!(
        "  mode: diverting={:?} ratchet_mode={:?} rotation_timeout={} short_long_timeout={} double_tap_speed={}",
        mode.diverting,
        mode.ratchet_mode,
        mode.rotation_timeout,
        mode.short_long_timeout,
        mode.double_tap_speed
    );
}
