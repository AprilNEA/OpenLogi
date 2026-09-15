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
    let written = openlogi_hid::set_pointer_scaling(&route, target).await;
    // Restore unconditionally and before judging the write: a failed
    // read-back can follow a write that already reached the device, and no
    // failed check may leave the pointer at the test speed.
    let restored = openlogi_hid::set_pointer_scaling(&route, before).await;
    let after = match (written, &restored) {
        (Ok(after), _) => after,
        (Err(write), Ok(_)) => {
            return Err(anyhow::Error::new(write)
                .context("write pointer scaling (original scaling restored)"));
        }
        (Err(write), Err(restore)) => anyhow::bail!(
            "write pointer scaling failed ({write}), and restoring {} failed too ({restore})",
            ScalingDisplay(before)
        ),
    };
    println!("  read-back scaling: {}", ScalingDisplay(after));

    let restored = restored.context("restore pointer scaling")?;
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

/// Half the current speed, far enough from it to be unmistakable. At the
/// smallest non-zero value, where halving would reach zero, it doubles instead
/// so the test never jumps to an unrelated speed.
fn default_target(current: PointerScaling) -> PointerScaling {
    let raw = current.raw();
    let target = if raw > 1 { raw / 2 } else { raw * 2 };
    PointerScaling::from_raw(target).unwrap_or(current)
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
    fn default_target_doubles_the_smallest_scaling_instead_of_jumping() {
        let smallest = PointerScaling::from_raw(1).unwrap();

        assert_eq!(default_target(smallest).raw(), 2);
    }

    #[test]
    fn default_target_halves_the_largest_scaling() {
        let largest = PointerScaling::from_raw(u16::MAX).unwrap();

        assert_eq!(default_target(largest).raw(), u16::MAX / 2);
    }

    #[test]
    fn displays_multiplier_and_raw_value() {
        assert_eq!(
            ScalingDisplay(PointerScaling::ONE).to_string(),
            "1.00× (0x0100)"
        );
    }
}
