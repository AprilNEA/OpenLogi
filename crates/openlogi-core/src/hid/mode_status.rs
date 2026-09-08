//! HID++ `ModeStatus` (feature `0x8090`) — the performance / endurance power
//! mode on G-series mice (G305 and friends).
//!
//! The protocol-level `0x8090` wrapper lives in `openlogi-hidpp`; this module
//! keeps OpenLogi's IPC-facing mode and capability types. Nothing here is
//! persisted to `config.toml`: the device owns the setting and keeps it across
//! power cycles, so the GUI only ever reads and writes the device.

use serde::{Deserialize, Serialize};

/// The power mode of a mouse with HID++ `0x8090 ModeStatus`.
///
/// Crosses the agent↔GUI IPC — serde encodes the variant *index*
/// (Endurance = 0, Performance = 1), so variant order is wire format and
/// changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerMode {
    /// Low-power mode: only the slowest report rate, months of battery life.
    Endurance,
    /// Performance mode: the full report-rate range at higher battery drain.
    Performance,
}

impl PowerMode {
    /// The opposite mode — used by the CLI diag round-trip toggle.
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Endurance => Self::Performance,
            Self::Performance => Self::Endurance,
        }
    }
}

/// Snapshot returned from OpenLogi's power-mode read helpers.
///
/// Crosses the agent↔GUI IPC (`read_power_mode`), so field order is wire
/// format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerModeState {
    /// Current mode.
    pub mode: PowerMode,
    /// Software can change the mode (`getDevConfig` bit 1). Gates the GUI
    /// toggle: a device without it only reports the mode.
    pub software_switch: bool,
    /// A hardware switch can change the mode (`getDevConfig` bit 0).
    pub hardware_switch: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flipped_is_an_involution() {
        assert_eq!(PowerMode::Endurance.flipped(), PowerMode::Performance);
        assert_eq!(PowerMode::Performance.flipped(), PowerMode::Endurance);
        assert_eq!(
            PowerMode::Endurance.flipped().flipped(),
            PowerMode::Endurance
        );
    }
}
