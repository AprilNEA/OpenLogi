//! `openlogi diag dpi` — DPI write round-trip.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::first_online_device;

#[derive(Debug, Args)]
pub struct DpiArgs {
    /// DPI to set during the test. Must be one of the values reported by the
    /// device's HID++ AdjustableDpi feature.
    #[arg(long)]
    pub target: Option<u16>,
}

pub async fn run(args: DpiArgs) -> Result<()> {
    let (route, name) = first_online_device().await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::get_dpi_info(&route)
        .await
        .context("read DPI capabilities")?;
    let before = info.current;
    println!("  current DPI: {before}");
    println!("  supported DPI: {}", summarize_dpi(&info.capabilities));

    let target = match args.target {
        Some(target) => {
            if !info.capabilities.contains(target) {
                anyhow::bail!(
                    "target {target} is not in the device-reported DPI list ({})",
                    summarize_dpi(&info.capabilities)
                );
            }
            target
        }
        None => info
            .capabilities
            .adjacent_test_target(before)
            .context("device reports fewer than two DPI values; pass --target to choose one")?,
    };
    if target == before {
        println!(
            "  target {target} equals current — pick a different --target to exercise the write"
        );
        return Ok(());
    }

    println!("  writing DPI: {target}");
    openlogi_hid::set_dpi(&route, target)
        .await
        .context("write DPI")?;

    let after = openlogi_hid::get_dpi(&route)
        .await
        .context("read DPI after write")?;
    println!("  read-back DPI: {after}");

    if after != target {
        anyhow::bail!(
            "DPI write failed: requested {target}, device reports {after} \
             (likely out of the device's supported range)"
        );
    }

    println!("  restoring DPI: {before}");
    openlogi_hid::set_dpi(&route, before)
        .await
        .context("restore DPI")?;

    println!("✓ DPI round-trip OK");
    Ok(())
}

fn summarize_dpi(capabilities: &openlogi_hid::DpiCapabilities) -> String {
    let values = capabilities.values();
    let step = capabilities.step_hint();
    if values.len() <= 12 {
        return values
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
    }
    format!(
        "{}..{} (step ≈ {step}, {} values)",
        capabilities.min(),
        capabilities.max(),
        values.len()
    )
}
