//! HID++ keyboard-backlight mode, status, and read-back state.
//!
//! These types describe the semantic state of HID++ `Backlight` feature
//! `0x1982`. The protocol conversion and transport operations that produce
//! them remain in `openlogi-device`.

use serde::{Deserialize, Serialize};

/// How the firmware decides the backlight brightness level.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant *index*, so
/// variant order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacklightMode {
    /// No mode selected.
    None,
    /// Level follows the ambient-light sensor.
    Automatic,
    /// Level adjusted with the keyboard's own backlight keys. The firmware
    /// enters this mode on its own; software cannot write it.
    TemporaryManual,
    /// Level set by software and held until changed.
    PermanentManual,
}

/// Why the backlight is in its current state, as reported by the device.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant *index*, so
/// variant order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacklightStatus {
    /// Turned off by software — the LEDs stay dark regardless of ambient
    /// light or hand proximity.
    DisabledBySoftware,
    /// Turned off because the battery is critically low.
    DisabledByCriticalBattery,
    /// Following the ambient-light sensor.
    AlsAutomatic,
    /// Following the ambient-light sensor, which reads bright enough that the
    /// LEDs are off.
    AlsSaturated,
    /// Holding a level the user picked with the backlight keys.
    TemporaryManual,
    /// Holding a level written by software.
    PermanentManual,
}

/// Snapshot of a keyboard's backlight, merged from the `0x1982`
/// `getBacklightConfig` and `getBacklightInfo` responses.
///
/// Crosses the agent↔GUI IPC, so field order is wire format — changes require
/// a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklightState {
    /// Whether the backlight system is enabled at all. When `false` the
    /// firmware keeps the LEDs dark no matter what the sensors report, and
    /// [`Self::status`] reads [`BacklightStatus::DisabledBySoftware`].
    pub enabled: bool,
    /// How the level is chosen while the backlight is enabled.
    pub mode: BacklightMode,
    /// Why the backlight is in its current state.
    pub status: BacklightStatus,
    /// Current brightness level, `0` (off) up to [`Self::nb_levels`] minus one.
    pub current_level: u8,
    /// Number of user-selectable brightness levels the device reports.
    pub nb_levels: u8,
}

impl BacklightState {
    /// Whether the LEDs are dark right now, for whatever reason — software
    /// disable, critical battery, a saturated ambient-light sensor, or a zero
    /// manual level.
    #[must_use]
    pub fn is_dark(self) -> bool {
        !self.enabled
            || self.current_level == 0
            || matches!(
                self.status,
                BacklightStatus::DisabledBySoftware
                    | BacklightStatus::DisabledByCriticalBattery
                    | BacklightStatus::AlsSaturated
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit() -> BacklightState {
        BacklightState {
            enabled: true,
            mode: BacklightMode::Automatic,
            status: BacklightStatus::AlsAutomatic,
            current_level: 4,
            nb_levels: 8,
        }
    }

    #[test]
    fn a_lit_backlight_is_not_dark() {
        assert!(!lit().is_dark());
    }

    #[test]
    fn software_disable_reads_as_dark() {
        let state = BacklightState {
            enabled: false,
            status: BacklightStatus::DisabledBySoftware,
            ..lit()
        };
        assert!(state.is_dark());
    }

    #[test]
    fn a_zero_level_reads_as_dark_even_while_enabled() {
        let state = BacklightState {
            current_level: 0,
            ..lit()
        };
        assert!(state.is_dark());
    }

    #[test]
    fn a_saturated_ambient_sensor_reads_as_dark() {
        let state = BacklightState {
            status: BacklightStatus::AlsSaturated,
            ..lit()
        };
        assert!(state.is_dark());
    }
}
