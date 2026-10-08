//! HID++ high-resolution wheel read-back state — pure data, no I/O.
//!
//! The HID++ `0x2121` conversions and reads/writes that produce this state
//! remain in `openlogi-device`.

use serde::{Deserialize, Serialize};

use crate::config::ScrollResolution;

/// Destination for vertical wheel movement reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollReportingTarget {
    /// Ordinary HID scroll reports delivered to the operating system.
    Native,
    /// HID++ notifications consumed by a host-side handler.
    Diverted,
}

/// Current HID++ `0x2121` wheel reporting mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollWheelMode {
    /// Vertical wheel reporting resolution.
    pub resolution: ScrollResolution,
    /// Whether native vertical reports are inverted in firmware.
    pub inverted: bool,
    /// Destination for wheel movement reports.
    pub target: ScrollReportingTarget,
}
