//! `openlogi diag crown` — read the HID++ `0x4600 Crown` capabilities and
//! current mode on the Craft's rotary crown, or write its ratchet mode.
//!
//! `get_info`/`get_mode` were the M2 smoke test confirming `crown.rs` decodes
//! real firmware payloads (previously only exercised against unit-test
//! fixtures); `--ratchet` extends that to the write path.

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use openlogi_hid::{CrownInfo, CrownMode, RatchetMode, SetCrownMode};

use crate::cmd::diag::select_device;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RatchetModeArg {
    /// Free-spinning mode.
    Free,
    /// Ratchet (detented) mode.
    Ratchet,
}

impl From<RatchetModeArg> for RatchetMode {
    fn from(value: RatchetModeArg) -> Self {
        match value {
            RatchetModeArg::Free => Self::Free,
            RatchetModeArg::Ratchet => Self::Ratchet,
        }
    }
}

#[derive(Debug, Args)]
pub struct CrownArgs {
    /// Ratchet mode to write directly to the device. This does not update
    /// config.toml; there is no persistent config for this yet.
    #[arg(long, value_enum)]
    pub ratchet: Option<RatchetModeArg>,

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

    let before = openlogi_hid::get_crown_mode(&route)
        .await
        .context("read Crown mode")?;
    print_mode("current", before);

    let Some(requested) = args.ratchet.map(RatchetMode::from) else {
        return Ok(());
    };

    let after = openlogi_hid::set_crown_mode(
        &route,
        SetCrownMode {
            diverting: None,
            ratchet_mode: Some(requested),
            rotation_timeout: None,
            short_long_timeout: None,
            double_tap_speed: None,
        },
    )
    .await
    .context("set Crown ratchet mode")?;
    print_mode("read-back", after);

    if after.ratchet_mode != requested {
        anyhow::bail!(
            "Crown ratchet mode write not applied: requested {requested:?}, device reports {:?}",
            after.ratchet_mode
        );
    }
    if after.diverting != before.diverting {
        anyhow::bail!(
            "Crown reporting mode changed unexpectedly: was {:?}, now {:?}",
            before.diverting,
            after.diverting
        );
    }

    println!("✓ Crown ratchet mode set to {requested:?} (reporting mode preserved)");
    Ok(())
}

fn print_info(info: CrownInfo) {
    println!(
        "  info: controls={:?} sensors={:?} slots={} ratchets={}",
        info.controls, info.sensors, info.slots, info.ratchets
    );
}

fn print_mode(label: &str, mode: CrownMode) {
    println!(
        "  {label}: diverting={:?} ratchet_mode={:?} rotation_timeout={} short_long_timeout={} double_tap_speed={}",
        mode.diverting,
        mode.ratchet_mode,
        mode.rotation_timeout,
        mode.short_long_timeout,
        mode.double_tap_speed
    );
}
