//! DPI read-back snapshot and capability math — pure data, no I/O.
//!
//! The HID++ reads/writes that produce a [`DpiInfo`] live in
//! `openlogi_hid::write::dpi`.

use std::num::TryFromIntError;

use az::SaturatingAs;
use nutype::nutype;
use serde::{Deserialize, Serialize};

use super::WriteError;

/// A sensor resolution that fits HID++'s unsigned 16-bit DPI field.
#[nutype(
    const_fn,
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        From,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct Dpi(u16);

impl Dpi {
    /// Round a floating-point control value into the DPI domain.
    #[must_use]
    pub fn from_rounded(value: f32) -> Self {
        Self::new(value.max(0.).round().saturating_as::<u16>())
    }
}

impl TryFrom<u32> for Dpi {
    type Error = TryFromIntError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        u16::try_from(value).map(Self::new)
    }
}

impl From<Dpi> for u32 {
    fn from(dpi: Dpi) -> Self {
        u32::from(dpi.into_inner())
    }
}

impl From<Dpi> for f32 {
    fn from(dpi: Dpi) -> Self {
        f32::from(dpi.into_inner())
    }
}

/// The span a settings slider offers: 200 to 8000 DPI on a 50-DPI grid, the
/// list an office mouse such as the MX Master 4 reports. See
/// [`DpiCapabilities::slider_stops`].
const SLIDER_SPAN: std::ops::RangeInclusive<u16> = 200..=8000;
const SLIDER_GRID: u16 = 50;

/// Supported DPI values reported by a device's HID++ AdjustableDpi feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiCapabilities {
    values: Vec<Dpi>,
}

impl DpiCapabilities {
    /// Build capabilities from a device-reported DPI list. Values are sorted
    /// and deduplicated so callers can rely on stable ordering.
    pub fn new(values: Vec<u16>) -> Result<Self, WriteError> {
        let mut values: Vec<Dpi> = values.into_iter().map(Dpi::from).collect();
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(WriteError::EmptyDpiList);
        }
        Ok(Self { values })
    }

    /// All supported DPI values, sorted ascending.
    #[must_use]
    pub fn values(&self) -> &[Dpi] {
        &self.values
    }

    /// Minimum supported DPI.
    #[must_use]
    pub fn min(&self) -> Dpi {
        self.values[0]
    }

    /// Maximum supported DPI.
    #[must_use]
    pub fn max(&self) -> Dpi {
        self.values[self.values.len() - 1]
    }

    /// Whether `dpi` is exactly supported by the device.
    #[must_use]
    pub fn contains(&self, dpi: Dpi) -> bool {
        self.values.binary_search(&dpi).is_ok()
    }

    /// The supported DPI nearest to `dpi`.
    #[must_use]
    pub fn nearest(&self, dpi: Dpi) -> Dpi {
        let mut nearest = self.values[0];
        let raw_dpi = dpi.into_inner();
        let mut best_delta = nearest.into_inner().abs_diff(raw_dpi);
        for &candidate in &self.values[1..] {
            let delta = candidate.into_inner().abs_diff(raw_dpi);
            if delta < best_delta {
                nearest = candidate;
                best_delta = delta;
            }
        }
        nearest
    }

    /// The values a settings slider stops at: the supported ones on the
    /// 50-DPI grid between 200 and 8000. A gaming sensor reports hundreds of
    /// values up to 48000 in steps down to 1, which no slider can be dragged
    /// across; an office mouse reports exactly this grid and keeps its list.
    /// Values outside it stay reachable by typing them. A list with fewer
    /// than two values on the grid is offered whole.
    #[must_use]
    pub fn slider_stops(&self) -> Self {
        let values: Vec<Dpi> = self
            .values
            .iter()
            .copied()
            .filter(|dpi| {
                let raw = dpi.into_inner();
                SLIDER_SPAN.contains(&raw) && raw % SLIDER_GRID == 0
            })
            .collect();
        if values.len() < 2 {
            self.clone()
        } else {
            Self { values }
        }
    }

    /// Best-effort step size for UI widgets that need a single increment.
    /// Returns the smallest positive gap between adjacent reported values.
    #[must_use]
    pub fn step_hint(&self) -> Dpi {
        self.values
            .array_windows::<2>()
            .filter_map(|&[low, high]| {
                high.into_inner()
                    .checked_sub(low.into_inner())
                    .map(Dpi::new)
            })
            .filter(|step| step.into_inner() > 0)
            .min()
            .unwrap_or(Dpi::new(1))
    }

    /// A supported value different from `current`, for diagnostic write tests.
    #[must_use]
    pub fn adjacent_test_target(&self, current: Dpi) -> Option<Dpi> {
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
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiInfo {
    /// DPI currently configured on sensor 0.
    pub current: Dpi,
    /// Supported values reported by the device for sensor 0.
    pub capabilities: DpiCapabilities,
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{Dpi, DpiCapabilities, WriteError};

    #[test]
    fn floating_control_values_round_and_stay_in_the_dpi_domain() {
        assert_eq!(Dpi::from_rounded(1599.6), Dpi::new(1600));
        assert_eq!(Dpi::from_rounded(-1.0), Dpi::new(0));
        assert_eq!(Dpi::from_rounded(70_000.0), Dpi::new(u16::MAX));
    }

    #[test]
    fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![1600, 400, 800, 800])?;

        assert_eq!(
            caps.values(),
            [Dpi::new(400), Dpi::new(800), Dpi::new(1600)]
        );
        assert_eq!(caps.min(), Dpi::new(400));
        assert_eq!(caps.max(), Dpi::new(1600));
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

        assert_eq!(caps.nearest(Dpi::new(390)), Dpi::new(400));
        assert_eq!(caps.nearest(Dpi::new(1000)), Dpi::new(800));
        assert_eq!(caps.nearest(Dpi::new(2000)), Dpi::new(1600));
        Ok(())
    }

    #[test]
    fn slider_stops_keep_the_supported_50_dpi_grid_up_to_8000() -> Result<(), WriteError> {
        // A gaming sensor: 100–48000 in steps that widen with the value.
        let mut gaming: Vec<u16> = (100..200).collect();
        gaming.extend((200..2000).step_by(2));
        gaming.extend((2000..5000).step_by(20));
        gaming.extend((5000..=48000).step_by(50));
        let stops = DpiCapabilities::new(gaming)?.slider_stops();

        assert_eq!(stops.min(), Dpi::new(200));
        assert_eq!(stops.max(), Dpi::new(8000));
        assert_eq!(stops.step_hint(), Dpi::new(50));
        assert!(stops.contains(Dpi::new(1950)));
        // 2050 is not a value the sensor takes; the slider steps past it.
        assert!(!stops.contains(Dpi::new(2050)));
        assert!(stops.contains(Dpi::new(2100)));
        Ok(())
    }

    #[test]
    fn slider_stops_leave_an_office_mouse_list_as_it_is() -> Result<(), WriteError> {
        let office = DpiCapabilities::new((200..=8000).step_by(50).collect())?;

        assert_eq!(office.slider_stops(), office);
        Ok(())
    }

    #[test]
    fn slider_stops_offer_a_list_off_the_grid_whole() -> Result<(), WriteError> {
        let coarse = DpiCapabilities::new(vec![125, 8000, 12000])?;

        assert_eq!(coarse.slider_stops(), coarse);
        Ok(())
    }

    #[test]
    fn step_hint_returns_smallest_positive_gap() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1200, 2000])?;

        assert_eq!(caps.step_hint(), Dpi::new(400));
        Ok(())
    }

    #[test]
    fn adjacent_test_target_prefers_next_then_previous_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(
            caps.adjacent_test_target(Dpi::new(400)),
            Some(Dpi::new(800))
        );
        assert_eq!(
            caps.adjacent_test_target(Dpi::new(800)),
            Some(Dpi::new(1600))
        );
        assert_eq!(
            caps.adjacent_test_target(Dpi::new(1600)),
            Some(Dpi::new(800))
        );
        Ok(())
    }

    #[test]
    fn adjacent_test_target_handles_current_outside_list() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(
            caps.adjacent_test_target(Dpi::new(1000)),
            Some(Dpi::new(1600))
        );
        assert_eq!(
            caps.adjacent_test_target(Dpi::new(2000)),
            Some(Dpi::new(1600))
        );
        Ok(())
    }
}
