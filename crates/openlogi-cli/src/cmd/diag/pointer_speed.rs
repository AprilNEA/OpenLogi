//! `openlogi diag pointer-speed` — PointerMotionScaling (`0x2205`) round-trip.

use std::{fmt, future::Future};

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::write::{PointerScaling, WriteError};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct PointerSpeedArgs {
    /// Raw 8.8 fixed-point scaling to write during the test (`256` is 1×).
    /// Defaults to half the current value. Firmware may store extreme values
    /// unclipped; the diagnostic always attempts to restore the original value.
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
    round_trip(before, target, |scaling| {
        openlogi_hid::set_pointer_scaling(&route, scaling)
    })
    .await
}

async fn round_trip<F, Fut>(
    before: PointerScaling,
    target: PointerScaling,
    mut set: F,
) -> Result<()>
where
    F: FnMut(PointerScaling) -> Fut,
    Fut: Future<Output = Result<PointerScaling, WriteError>>,
{
    if target == before {
        println!("  target equals current — pick a different --target to exercise the write");
        return Ok(());
    }

    println!("  writing scaling: {}", ScalingDisplay(target));
    let written = set(target).await;
    // Restore unconditionally and before judging the write: a failed
    // read-back can follow a write that already reached the device, and no
    // failed check may leave the pointer at the test speed.
    let restored = set(before)
        .await
        .context("restore pointer scaling")
        .and_then(|restored| {
            anyhow::ensure!(
                restored == before,
                "restore failed: expected {}, device reports {}",
                ScalingDisplay(before),
                ScalingDisplay(restored)
            );
            Ok(restored)
        });
    let after = match (written, &restored) {
        (Ok(after), _) => after,
        (Err(write), Ok(_)) => {
            return Err(anyhow::Error::new(write)
                .context("write pointer scaling (original scaling restored)"));
        }
        (Err(write), Err(restore)) => anyhow::bail!(
            "write pointer scaling failed ({write}), and restoring {} failed too ({restore:#})",
            ScalingDisplay(before)
        ),
    };
    println!("  read-back scaling: {}", ScalingDisplay(after));

    let restored = restored?;
    println!("  restored scaling: {}", ScalingDisplay(restored));

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
    use openlogi_hid::write::{PointerScaling, WriteError};

    use super::{ScalingDisplay, default_target, round_trip};

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

    #[tokio::test]
    async fn reports_failed_restore_when_the_test_write_also_fails() {
        let before = PointerScaling::ONE;
        let target = PointerScaling::from_raw(0x0080).unwrap();
        let mut replies = [
            Err(WriteError::Hid("test read-back failed".into())),
            Ok(target),
        ]
        .into_iter();
        let mut writes = Vec::new();

        let error = round_trip(before, target, |value| {
            writes.push(value);
            std::future::ready(replies.next().unwrap())
        })
        .await
        .unwrap_err();

        assert_eq!(writes, [target, before]);
        let message = format!("{error:#}");
        assert!(message.contains("test read-back failed"), "{message}");
        assert!(message.contains("restore failed"), "{message}");
        assert!(message.contains("0x0100"), "{message}");
        assert!(message.contains("0x0080"), "{message}");
        assert!(!message.contains("original scaling restored"), "{message}");
    }

    #[tokio::test]
    async fn restores_before_rejecting_an_unchanged_write() {
        let before = PointerScaling::ONE;
        let target = PointerScaling::from_raw(0x0080).unwrap();
        let mut writes = Vec::new();

        let error = round_trip(before, target, |value| {
            writes.push(value);
            std::future::ready(Ok(before))
        })
        .await
        .unwrap_err();

        assert_eq!(writes, [target, before]);
        assert!(error.to_string().contains("write had no effect"), "{error}");
    }

    #[tokio::test]
    async fn preserves_write_and_restore_outcomes() {
        let before = PointerScaling::ONE;
        let target = PointerScaling::from_raw(0x0080).unwrap();
        let clipped = PointerScaling::from_raw(0x0040).unwrap();
        let cases = [
            (Ok(target), Ok(before), None),
            (Ok(clipped), Ok(before), None),
            (
                Err(WriteError::Hid("test write failed".into())),
                Ok(before),
                Some(
                    "write pointer scaling (original scaling restored): HID transport error: test write failed",
                ),
            ),
            (
                Ok(target),
                Err(WriteError::Hid("restore failed on transport".into())),
                Some("restore pointer scaling: HID transport error: restore failed on transport"),
            ),
            (
                Err(WriteError::Hid("test write failed".into())),
                Err(WriteError::Hid("restore failed on transport".into())),
                Some(
                    "write pointer scaling failed (HID transport error: test write failed), and restoring 1.00× (0x0100) failed too (restore pointer scaling: HID transport error: restore failed on transport)",
                ),
            ),
            (
                Ok(target),
                Ok(target),
                Some("restore failed: expected 1.00× (0x0100), device reports 0.50× (0x0080)"),
            ),
        ];

        for (written, restored, expected_error) in cases {
            let mut replies = [written, restored].into_iter();
            let mut writes = Vec::new();
            let result = round_trip(before, target, |value| {
                writes.push(value);
                std::future::ready(replies.next().unwrap())
            })
            .await;

            assert_eq!(writes, [target, before]);
            if let Some(expected) = expected_error {
                assert_eq!(format!("{:#}", result.unwrap_err()), expected);
            } else {
                result.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn matching_target_does_not_write() {
        let mut writes = 0;
        round_trip(PointerScaling::ONE, PointerScaling::ONE, |_| {
            writes += 1;
            std::future::ready(Ok(PointerScaling::ONE))
        })
        .await
        .unwrap();
        assert_eq!(writes, 0);
    }
}
