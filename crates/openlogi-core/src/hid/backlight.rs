//! HID++ keyboard-backlight mode, status, read-back state, and changes.
//!
//! These types describe the semantic state of HID++ `Backlight` feature
//! `0x1982`. The protocol conversion and transport operations that produce
//! them remain in `openlogi-device`.

use std::fmt;

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
    /// Whether battery saving is on: the firmware turns the backlight off
    /// when the battery runs low.
    ///
    /// `serde(default)` only keeps recorded JSON fixtures that predate the
    /// field loading; the IPC encoding always carries it.
    #[serde(default)]
    pub power_save: bool,
    /// Which [`BacklightChange`]s the keyboard accepts.
    #[serde(default)]
    pub controls: BacklightControls,
}

/// What software may change on a keyboard backlight beyond turning it on or
/// off, from the `0x1982` capability bits and feature version.
///
/// Crosses the agent↔GUI IPC inside [`BacklightState`], so field order is
/// wire format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklightControls {
    /// Software can switch between the ambient-light sensor choosing the
    /// level and holding one (permanent-manual mode): the keyboard supports
    /// both.
    pub automatic: bool,
    /// Software can pick the level: permanent-manual mode, `0x1982` version 3
    /// or later, and more than one level.
    pub level: bool,
    /// The firmware can turn the backlight off on a low battery.
    pub power_save: bool,
}

impl BacklightControls {
    /// Whether `change` is one this keyboard accepts, ignoring the level
    /// range: [`BacklightChange::Level`] also needs a level below
    /// [`BacklightState::nb_levels`].
    #[must_use]
    pub fn allow(self, change: BacklightChange) -> bool {
        match change {
            BacklightChange::Enabled(_) => true,
            BacklightChange::Automatic(_) => self.automatic,
            BacklightChange::Level(_) => self.level,
            BacklightChange::PowerSave(_) => self.power_save,
        }
    }
}

/// One change to a keyboard backlight, written to the keyboard's own memory.
///
/// Crosses the agent↔GUI IPC, where serde encodes the variant *index*, so
/// variant order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BacklightChange {
    /// Turn the backlight system on or off.
    Enabled(bool),
    /// Let the ambient-light sensor choose the level (`true`), or hold the
    /// level the keyboard shows now (`false`), so nothing visibly changes.
    Automatic(bool),
    /// Hold this level, `0` (off) up to [`BacklightState::nb_levels`] minus
    /// one. Leaves automatic mode.
    Level(u8),
    /// Turn battery saving on or off.
    PowerSave(bool),
}

impl fmt::Display for BacklightChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on_off = |on: bool| if on { "on" } else { "off" };
        match *self {
            Self::Enabled(on) => write!(f, "backlight {}", on_off(on)),
            Self::Automatic(on) => write!(f, "automatic brightness {}", on_off(on)),
            Self::Level(level) => write!(f, "backlight level {level}"),
            Self::PowerSave(on) => write!(f, "battery saving {}", on_off(on)),
        }
    }
}

impl BacklightState {
    /// Whether the keyboard accepts `change` right now: the control exists
    /// and, for a level, the level is in range.
    #[must_use]
    pub fn accepts(self, change: BacklightChange) -> bool {
        self.controls.allow(change)
            && match change {
                BacklightChange::Level(level) => level < self.nb_levels,
                _ => true,
            }
    }

    /// Whether the ambient-light sensor chooses the level — the firmware's
    /// automatic mode, or the temporary level the backlight keys set on top
    /// of it.
    #[must_use]
    pub fn is_automatic(self) -> bool {
        matches!(
            self.mode,
            BacklightMode::Automatic | BacklightMode::TemporaryManual
        )
    }

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
            power_save: true,
            controls: BacklightControls {
                automatic: true,
                level: true,
                power_save: true,
            },
        }
    }

    #[test]
    fn every_change_needs_its_control() {
        let all = lit();
        let none = BacklightState {
            controls: BacklightControls::default(),
            ..lit()
        };
        for change in [
            BacklightChange::Automatic(false),
            BacklightChange::Level(3),
            BacklightChange::PowerSave(false),
        ] {
            assert!(all.accepts(change), "{change}");
            assert!(!none.accepts(change), "{change}");
        }
        // On/off is part of every 0x1982 keyboard.
        assert!(none.accepts(BacklightChange::Enabled(false)));
    }

    #[test]
    fn a_level_must_be_below_the_level_count() {
        assert!(lit().accepts(BacklightChange::Level(0)));
        assert!(lit().accepts(BacklightChange::Level(7)));
        assert!(!lit().accepts(BacklightChange::Level(8)));
    }

    #[test]
    fn the_backlight_keys_leave_the_sensor_in_charge() {
        let keys = BacklightState {
            mode: BacklightMode::TemporaryManual,
            ..lit()
        };
        let held = BacklightState {
            mode: BacklightMode::PermanentManual,
            ..lit()
        };
        assert!(lit().is_automatic());
        assert!(keys.is_automatic());
        assert!(!held.is_automatic());
    }

    #[test]
    fn changes_name_themselves_for_error_messages() {
        assert_eq!(BacklightChange::Enabled(true).to_string(), "backlight on");
        assert_eq!(
            BacklightChange::Automatic(false).to_string(),
            "automatic brightness off"
        );
        assert_eq!(BacklightChange::Level(3).to_string(), "backlight level 3");
        assert_eq!(
            BacklightChange::PowerSave(true).to_string(),
            "battery saving on"
        );
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
