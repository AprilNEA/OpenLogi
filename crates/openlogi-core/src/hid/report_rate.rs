//! Report-rate read-back snapshot — pure data, no I/O.
//!
//! The HID++ reads and writes that produce a [`ReportRateInfo`] live in
//! `openlogi_device::write::report_rate`.

use nutype::nutype;
use serde::{Deserialize, Serialize};

/// How often a device sends movement reports, in whole hertz.
///
/// HID++ `0x8060 AdjustableReportRate` sets a report *interval* of 1–8 ms and
/// `0x8061 ExtendedAdjustableReportRate` one of seven rates from 125 Hz to
/// 8 kHz. Both map onto whole hertz rounded to the nearest (a 3 ms interval
/// is 333 Hz), and no two settings of either feature round to the same
/// value, so a rate names exactly one setting on the wire.
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
        Hash,
        From,
        Into,
        Display,
        Serialize,
        Deserialize
    )
)]
pub struct ReportRate(u16);

/// A device's report rate and the rates it lets software choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRateInfo {
    /// The rate the device reports at now.
    pub current: ReportRate,
    /// Every rate the device accepts on its current connection, ascending
    /// and without duplicates. The device layer never builds an empty list.
    pub supported: Vec<ReportRate>,
}

impl ReportRateInfo {
    /// Whether the device accepts `rate` on its current connection.
    #[must_use]
    pub fn supports(&self, rate: ReportRate) -> bool {
        self.supported.contains(&rate)
    }
}

#[cfg(test)]
mod tests {
    use super::{ReportRate, ReportRateInfo};

    #[test]
    fn a_rate_is_whole_hertz() {
        let rate = ReportRate::new(1000);
        assert_eq!(u16::from(rate), 1000);
        assert_eq!(rate.to_string(), "1000");
    }

    #[test]
    fn supports_only_listed_rates() {
        let info = ReportRateInfo {
            current: ReportRate::new(1000),
            supported: vec![ReportRate::new(125), ReportRate::new(1000)],
        };
        assert!(info.supports(ReportRate::new(125)));
        assert!(info.supports(ReportRate::new(1000)));
        assert!(!info.supports(ReportRate::new(500)));
    }
}
