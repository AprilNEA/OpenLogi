//! `openlogi diag dpi` — DPI write round-trip.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct DpiArgs {
    /// DPI to set during the test. Must be one of the values reported by the
    /// device's HID++ AdjustableDpi feature.
    #[arg(long)]
    pub target: Option<u16>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: DpiArgs) -> Result<()> {
    // 0x2201 = AdjustableDpi — auto-skip devices (keyboards) that lack it.
    let (route, name) = select_device(args.device.as_deref(), &[0x2201]).await?;
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

    // `target` is always a device-reported value, so a mismatch means the
    // device adjusted it — fine if it landed on another supported value, but a
    // no-op write (`after == before`) or an off-list read-back is a real fault.
    // (`target != before` is guaranteed by the early return above.)
    if after == before {
        anyhow::bail!("DPI write failed: requested {target}, device still reports {before}");
    }
    if after != target {
        if info.capabilities.contains(after) {
            println!("  note: device snapped {target} → {after}");
        } else {
            anyhow::bail!(
                "DPI write failed: requested {target}, device reports {after} \
                 which is not in its supported list"
            );
        }
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
