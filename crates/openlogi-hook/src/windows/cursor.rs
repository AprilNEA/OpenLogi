//! Typed Windows cursor normalization, independent of native DPI queries.

use std::num::{NonZeroU32, TryFromIntError};

use crate::CursorPosition;

/// Windows defines one logical inch as 96 DIPs (`USER_DEFAULT_SCREEN_DPI`).
const LOGICAL_DPI: NonZeroU32 = NonZeroU32::new(96).expect("96 DPI is nonzero");

/// A global physical-pixel cursor sample from a DPI-aware Windows process.
pub(super) struct PhysicalCursorPosition {
    pub(super) x: i32,
    pub(super) y: i32,
}

impl PhysicalCursorPosition {
    /// Use the DPI of the monitor resolved from this same physical sample.
    pub(super) fn into_logical(self, dpi: MonitorDpi) -> CursorPosition {
        let base = f64::from(LOGICAL_DPI.get());
        CursorPosition {
            x: f64::from(self.x) / (f64::from(dpi.x.get()) / base),
            y: f64::from(self.y) / (f64::from(dpi.y.get()) / base),
        }
    }
}

/// Effective monitor DPI with neither axis allowed to be zero.
#[derive(Clone, Copy, Debug)]
pub(super) struct MonitorDpi {
    x: NonZeroU32,
    y: NonZeroU32,
}

impl TryFrom<(u32, u32)> for MonitorDpi {
    type Error = TryFromIntError;

    fn try_from((x, y): (u32, u32)) -> Result<Self, Self::Error> {
        Ok(Self {
            x: NonZeroU32::try_from(x)?,
            y: NonZeroU32::try_from(y)?,
        })
    }
}

impl Default for MonitorDpi {
    fn default() -> Self {
        Self {
            x: LOGICAL_DPI,
            y: LOGICAL_DPI,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_position_uses_each_axis_scale_without_rounding() {
        for (raw_dpi, expected) in [
            ((96, 96), (1501.0, -903.0)),
            ((144, 144), (1_000.666_666_666_666_6, -602.0)),
            ((192, 144), (750.5, -602.0)),
        ] {
            let physical = PhysicalCursorPosition { x: 1501, y: -903 };
            let logical = physical.into_logical(MonitorDpi::try_from(raw_dpi).unwrap());
            assert!((logical.x - expected.0).abs() < 1e-9, "DPI: {raw_dpi:?}");
            assert!((logical.y - expected.1).abs() < 1e-9, "DPI: {raw_dpi:?}");
        }
    }

    #[test]
    fn default_dpi_preserves_global_coordinates() {
        let physical = PhysicalCursorPosition {
            x: i32::MIN,
            y: i32::MAX,
        };
        assert_eq!(
            physical.into_logical(MonitorDpi::default()),
            CursorPosition {
                x: -2_147_483_648.0,
                y: 2_147_483_647.0,
            }
        );
    }

    #[test]
    fn zero_on_either_axis_rejects_the_pair_and_falls_back_on_both_axes() {
        for raw_dpi in [(0, 144), (192, 0), (0, 0)] {
            let dpi = MonitorDpi::try_from(raw_dpi);
            dpi.expect_err("either zero axis must reject the whole pair");
            let physical = PhysicalCursorPosition { x: -1501, y: 903 };
            assert_eq!(
                physical.into_logical(dpi.unwrap_or_default()),
                CursorPosition {
                    x: -1501.0,
                    y: 903.0,
                }
            );
        }
    }

    #[test]
    fn dpi_validation_accepts_the_full_nonzero_range() {
        let dpi = MonitorDpi::try_from((1, u32::MAX)).unwrap();
        assert_eq!(dpi.x.get(), 1);
        assert_eq!(dpi.y.get(), u32::MAX);
    }
}
