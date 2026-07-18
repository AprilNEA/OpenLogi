//! `openlogi diag wheel` — inspect or set the HiResWheel reporting mode.

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use openlogi_hid::{ScrollReportingTarget, ScrollResolution, ScrollWheelMode};

use crate::cmd::diag::select_device;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ResolutionArg {
    /// One report per physical ratchet step.
    Low,
    /// Fine-grained reports between ratchet steps.
    High,
}

impl From<ResolutionArg> for ScrollResolution {
    fn from(value: ResolutionArg) -> Self {
        match value {
            ResolutionArg::Low => Self::Low,
            ResolutionArg::High => Self::High,
        }
    }
}

#[derive(Debug, Args)]
pub struct WheelArgs {
    /// Resolution to write directly to the device. This does not update
    /// config.toml; use the GUI or TOML for persistent reconnect behavior.
    #[arg(long, value_enum)]
    pub resolution: Option<ResolutionArg>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: WheelArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x2121]).await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_scroll_wheel_mode(&route)
        .await
        .context("read HiResWheel mode")?;
    print_mode("current", before);

    let Some(requested) = args.resolution.map(ScrollResolution::from) else {
        return Ok(());
    };

    openlogi_hid::set_scroll_resolution(&route, requested)
        .await
        .context("set wheel resolution")?;
    let after = openlogi_hid::get_scroll_wheel_mode(&route)
        .await
        .context("read HiResWheel mode after write")?;
    print_mode("read-back", after);

    if after.resolution != requested {
        anyhow::bail!(
            "wheel resolution write not applied: requested {}, device reports {}",
            resolution_label(requested),
            resolution_label(after.resolution)
        );
    }
    if after.target != ScrollReportingTarget::Native {
        anyhow::bail!(
            "wheel reporting target is not native after write: {:?}",
            after.target
        );
    }
    if after.inverted != before.inverted {
        anyhow::bail!(
            "wheel inversion changed unexpectedly: was {}, now {}",
            before.inverted,
            after.inverted
        );
    }

    println!(
        "✓ wheel resolution set to {} (native reporting, inversion preserved)",
        resolution_label(requested)
    );
    Ok(())
}

fn print_mode(label: &str, mode: ScrollWheelMode) {
    println!(
        "  {label}: resolution={} inversion={} reporting={}",
        resolution_label(mode.resolution),
        if mode.inverted { "inverted" } else { "normal" },
        match mode.target {
            ScrollReportingTarget::Native => "native",
            ScrollReportingTarget::Diverted => "diverted",
        }
    );
}

fn resolution_label(resolution: ScrollResolution) -> &'static str {
    match resolution {
        ScrollResolution::Low => "low",
        ScrollResolution::High => "high",
    }
}
