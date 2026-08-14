//! DPI read-back snapshot and capability math — pure data, no I/O.
//!
//! The HID++ reads/writes that produce a [`DpiInfo`] live in
//! `openlogi_hid::write::dpi`.

use serde::{Deserialize, Serialize};

use super::WriteError;

/// Supported DPI values reported by a device's HID++ AdjustableDpi feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiCapabilities {
    values: Vec<u16>,
}

impl DpiCapabilities {
    /// Build capabilities from a device-reported DPI list. Values are sorted
    /// and deduplicated so callers can rely on stable ordering.
    pub fn new(mut values: Vec<u16>) -> Result<Self, WriteError> {
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(WriteError::EmptyDpiList);
        }
        Ok(Self { values })
    }

    /// All supported DPI values, sorted ascending.
    #[must_use]
    pub fn values(&self) -> &[u16] {
        &self.values
    }

    /// Minimum supported DPI.
    #[must_use]
    pub fn min(&self) -> u16 {
        self.values[0]
    }

    /// Maximum supported DPI.
    #[must_use]
    pub fn max(&self) -> u16 {
        self.values[self.values.len() - 1]
    }

    /// Whether `dpi` is exactly supported by the device.
    #[must_use]
    pub fn contains(&self, dpi: u16) -> bool {
        self.values.binary_search(&dpi).is_ok()
    }

    /// The supported DPI nearest to `dpi`.
    #[must_use]
    pub fn nearest(&self, dpi: u32) -> u16 {
        let mut nearest = self.values[0];
        let mut best_delta = u32::from(nearest).abs_diff(dpi);
        for &candidate in &self.values[1..] {
            let delta = u32::from(candidate).abs_diff(dpi);
            if delta < best_delta {
                nearest = candidate;
                best_delta = delta;
            }
        }
        nearest
    }

    /// Snap `dpi` to the nearest supported value, widened to `u32` for UI math.
    /// The single home for "round a DPI onto this device's grid" — callers that
    /// hold an `Option<DpiCapabilities>` should `map_or(dpi, |c| c.snap(dpi))`.
    #[must_use]
    pub fn snap(&self, dpi: u32) -> u32 {
        u32::from(self.nearest(dpi))
    }

    /// Best-effort step size for UI widgets that need a single increment.
    /// Returns the smallest positive gap between adjacent reported values.
    #[must_use]
    pub fn step_hint(&self) -> u16 {
        self.values
            .array_windows::<2>()
            .filter_map(|&[low, high]| high.checked_sub(low))
            .filter(|step| *step > 0)
            .min()
            .unwrap_or(1)
    }

    /// A supported value different from `current`, for diagnostic write tests.
    #[must_use]
    pub fn adjacent_test_target(&self, current: u16) -> Option<u16> {
        if self.values.len() < 2 {
            return None;
        }
        match self.values.binary_search(&current) {
            Ok(index) if index + 1 < self.values.len() => Some(self.values[index + 1]),
            Ok(index) if index > 0 => Some(self.values[index - 1]),
            Ok(_) => None,
            Err(index) if index < self.values.len() => Some(self.values[index]),
            Err(_) => self.values.last().copied(),
        }
        .filter(|target| *target != current)
    }
}

/// Current DPI plus the supported values reported by the device.
///
/// Crosses the agent↔GUI IPC (`read_dpi`, [`DpiCapabilities`] included), so
/// field order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiInfo {
    /// DPI currently configured on sensor 0.
    pub current: u16,
    /// Supported values reported by the device for sensor 0.
    pub capabilities: DpiCapabilities,
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{DpiCapabilities, WriteError};

    #[test]
    fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![1600, 400, 800, 800])?;

        assert_eq!(caps.values(), [400, 800, 1600]);
        assert_eq!(caps.min(), 400);
        assert_eq!(caps.max(), 1600);
        Ok(())
    }

    #[test]
    fn capabilities_reject_empty_list() {
        assert_matches!(
            DpiCapabilities::new(Vec::new()),
            Err(WriteError::EmptyDpiList)
        );
    }

    #[test]
    fn nearest_returns_closest_supported_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.nearest(390), 400);
        assert_eq!(caps.nearest(1000), 800);
        assert_eq!(caps.nearest(2000), 1600);
        Ok(())
    }

    #[test]
    fn step_hint_returns_smallest_positive_gap() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1200, 2000])?;

        assert_eq!(caps.step_hint(), 400);
        Ok(())
    }

    #[test]
    fn adjacent_test_target_prefers_next_then_previous_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.adjacent_test_target(400), Some(800));
        assert_eq!(caps.adjacent_test_target(800), Some(1600));
        assert_eq!(caps.adjacent_test_target(1600), Some(800));
        Ok(())
    }

    #[test]
    fn adjacent_test_target_handles_current_outside_list() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.adjacent_test_target(1000), Some(1600));
        assert_eq!(caps.adjacent_test_target(2000), Some(1600));
        Ok(())
    }
}
