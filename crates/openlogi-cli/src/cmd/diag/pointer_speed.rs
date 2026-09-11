//! `openlogi diag pointer-speed` — PointerMotionScaling (`0x2205`) round-trip.

use std::fmt;

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::write::PointerScaling;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct PointerSpeedArgs {
    /// Raw 8.8 fixed-point scaling to write during the test (`256` is 1×).
    /// Defaults to half the current value. Firmware may store extreme values
    /// unclipped; the original value is restored either way.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub target: Option<u16>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: PointerSpeedArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x2205]).await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_pointer_scaling(&route)
        .await
        .context("read pointer scaling")?;
    println!("  current scaling: {}", ScalingDisplay(before));

    let target = match args.target {
        Some(raw) => PointerScaling::from_raw(raw).context("--target must be non-zero")?,
        None => default_target(before),
    };
    if target == before {
        println!("  target equals current — pick a different --target to exercise the write");
        return Ok(());
    }

    println!("  writing scaling: {}", ScalingDisplay(target));
    let after = openlogi_hid::set_pointer_scaling(&route, target)
        .await
        .context("write pointer scaling")?;
    println!("  read-back scaling: {}", ScalingDisplay(after));

    // Restore before judging the write, so a failed check never leaves the
    // pointer at the test speed.
    let restored = openlogi_hid::set_pointer_scaling(&route, before)
        .await
        .context("restore pointer scaling")?;
    println!("  restored scaling: {}", ScalingDisplay(restored));
    if restored != before {
        anyhow::bail!(
            "restore failed: expected {}, device reports {}",
            ScalingDisplay(before),
            ScalingDisplay(restored)
        );
    }

    if after == before {
        anyhow::bail!(
            "pointer scaling write had no effect: requested {}, device still reports {}",
            ScalingDisplay(target),
            ScalingDisplay(before)
        );
    }
    if after != target {
        println!(
            "  note: device clipped {} → {}",
            ScalingDisplay(target),
            ScalingDisplay(after)
        );
    }

    println!("✓ pointer scaling round-trip OK");
    Ok(())
}

/// Half the current speed: far enough from it to be unmistakable, and always
/// inside the range firmware accepts for a device currently at a sane value.
fn default_target(current: PointerScaling) -> PointerScaling {
    PointerScaling::from_raw(current.raw() / 2).unwrap_or(PointerScaling::ONE)
}

struct ScalingDisplay(PointerScaling);

impl fmt::Display for ScalingDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}× (0x{:04x})", self.0.multiplier(), self.0.raw())
    }
}

#[cfg(test)]
mod tests {
    use openlogi_hid::write::PointerScaling;

    use super::{ScalingDisplay, default_target};

    #[test]
    fn default_target_halves_the_current_scaling() {
        assert_eq!(default_target(PointerScaling::ONE).raw(), 0x0080);
    }

    #[test]
    fn default_target_never_writes_zero() {
        let smallest = PointerScaling::from_raw(1).unwrap();

        assert_eq!(default_target(smallest), PointerScaling::ONE);
    }

    #[test]
    fn displays_multiplier_and_raw_value() {
        assert_eq!(
            ScalingDisplay(PointerScaling::ONE).to_string(),
            "1.00× (0x0100)"
        );
    }
}
