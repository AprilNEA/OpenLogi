//! Report-rate read-back snapshot and capability math — pure data, no I/O.
//!
//! The HID++ reads/writes that produce a [`ReportRateInfo`] live in
//! `openlogi_device::write::report_rate`.

use std::num::TryFromIntError;

use nutype::nutype;
use serde::{Deserialize, Serialize};

use super::WriteError;

/// A polling frequency in hertz reported by HID++ AdjustableReportRate features.
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
pub struct ReportRateHz(u16);

impl ReportRateHz {
    /// Convert a legacy `0x8060` report interval in milliseconds to hertz.
    #[must_use]
    pub fn from_interval_ms(ms: u8) -> Option<Self> {
        if ms == 0 || ms > 8 {
            return None;
        }
        Some(Self::new(1000 / u16::from(ms)))
    }
}

impl TryFrom<u32> for ReportRateHz {
    type Error = TryFromIntError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(Self::new(u16::try_from(value)?))
    }
}

/// Supported report rates reported by a device's HID++ report-rate feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRateCapabilities {
    values: Vec<ReportRateHz>,
}

impl ReportRateCapabilities {
    /// Build capabilities from a device-reported Hz list. Values are sorted
    /// and deduplicated so callers can rely on stable ordering.
    pub fn new(values: Vec<u16>) -> Result<Self, WriteError> {
        let mut values: Vec<ReportRateHz> = values
            .into_iter()
            .filter(|&hz| hz > 0)
            .map(ReportRateHz::new)
            .collect();
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(WriteError::EmptyReportRateList);
        }
        Ok(Self { values })
    }

    /// All supported rates, sorted ascending.
    #[must_use]
    pub fn values(&self) -> &[ReportRateHz] {
        &self.values
    }

    /// Minimum supported rate.
    #[must_use]
    pub fn min(&self) -> ReportRateHz {
        self.values[0]
    }

    /// Maximum supported rate.
    #[must_use]
    pub fn max(&self) -> ReportRateHz {
        self.values[self.values.len() - 1]
    }

    /// Whether `rate` is exactly supported by the device.
    #[must_use]
    pub fn contains(&self, rate: ReportRateHz) -> bool {
        self.values.binary_search(&rate).is_ok()
    }

    /// The supported rate nearest to `rate`.
    #[must_use]
    pub fn nearest(&self, rate: ReportRateHz) -> ReportRateHz {
        let mut nearest = self.values[0];
        let raw = rate.into_inner();
        let mut best_delta = nearest.into_inner().abs_diff(raw);
        for &candidate in &self.values[1..] {
            let delta = candidate.into_inner().abs_diff(raw);
            if delta < best_delta {
                nearest = candidate;
                best_delta = delta;
            }
        }
        nearest
    }
}

/// Current report rate plus the supported values reported by the device.
///
/// Crosses the agent↔GUI IPC (`read_report_rate`, [`ReportRateCapabilities`]
/// included), so field order is wire format — changes require a
/// `PROTOCOL_VERSION` bump (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRateInfo {
    /// Report rate currently configured on the device.
    pub current: ReportRateHz,
    /// Supported values reported by the device.
    pub capabilities: ReportRateCapabilities,
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::{ReportRateCapabilities, ReportRateHz, WriteError};

    #[test]
    fn interval_ms_converts_to_hertz() {
        assert_eq!(
            ReportRateHz::from_interval_ms(1),
            Some(ReportRateHz::new(1000))
        );
        assert_eq!(
            ReportRateHz::from_interval_ms(8),
            Some(ReportRateHz::new(125))
        );
        assert_eq!(ReportRateHz::from_interval_ms(0), None);
    }

    #[test]
    fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
        let caps = ReportRateCapabilities::new(vec![1000, 125, 500, 500])?;

        assert_eq!(
            caps.values(),
            [
                ReportRateHz::new(125),
                ReportRateHz::new(500),
                ReportRateHz::new(1000),
            ]
        );
        Ok(())
    }

    #[test]
    fn capabilities_reject_empty_list() {
        assert_matches!(
            ReportRateCapabilities::new(Vec::new()),
            Err(WriteError::EmptyReportRateList)
        );
    }

    #[test]
    fn nearest_returns_closest_supported_value() -> Result<(), WriteError> {
        let caps = ReportRateCapabilities::new(vec![125, 500, 1000])?;

        assert_eq!(caps.nearest(ReportRateHz::new(130)), ReportRateHz::new(125));
        assert_eq!(caps.nearest(ReportRateHz::new(800)), ReportRateHz::new(1000));
        assert_eq!(
            caps.nearest(ReportRateHz::new(2000)),
            ReportRateHz::new(1000)
        );
        Ok(())
    }
}
