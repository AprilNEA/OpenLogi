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
    /// How long the keys stay lit after typing stops, in seconds.
    ///
    /// From the keyboard's hands-out fade-out, which Logi Options+ shows as
    /// the backlight timer. `0` when the keyboard does not offer a duration or
    /// reports none. A value outside the steps the panel is showing is still
    /// reported as stored — a longer timer stays visible instead of being
    /// snapped to 30 seconds.
    ///
    /// `serde(default)` only keeps recorded JSON fixtures that predate the
    /// field loading; the IPC encoding always carries it.
    #[serde(default)]
    pub duration_secs: u16,
}

/// What software may change on a keyboard backlight beyond turning it on or
/// off, from the `0x1982` capability bits and feature version.
///
/// Crosses the agent↔GUI IPC inside [`BacklightState`], so field order is
/// wire format — changes require a `PROTOCOL_VERSION` bump (guarded by
/// `openlogi-ipc/tests/wire_format.rs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent offers from 0x1982, not a state machine; bools keep the IPC shape explicit"
)]
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
    /// Software can set how long the keys stay lit after typing stops:
    /// `0x1982` version 3 or later, which is the layout those durations are
    /// written in.
    ///
    /// `serde(default)` only keeps recorded JSON fixtures that predate the
    /// field loading; the IPC encoding always carries it.
    #[serde(default)]
    pub duration: bool,
}

impl BacklightControls {
    /// Whether `change` is one this keyboard accepts, ignoring the level
    /// range and the duration range: [`BacklightChange::Level`] also needs a
    /// level below [`BacklightState::nb_levels`], and
    /// [`BacklightChange::Duration`] a multiple of five seconds up to two hours.
    #[must_use]
    pub fn allow(self, change: BacklightChange) -> bool {
        match change {
            BacklightChange::Enabled(_) => true,
            BacklightChange::Automatic(_) => self.automatic,
            BacklightChange::Level(_) => self.level,
            BacklightChange::PowerSave(_) => self.power_save,
            BacklightChange::Duration(_) => self.duration,
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
    /// Keep the keys lit for this many seconds after typing stops. A multiple
    /// of [`Self::DURATION_STEP_SECONDS`] from that step through
    /// [`Self::MAX_DURATION_SECONDS`] — one unit through `0x05a0`, the longest
    /// fade-out `0x1982` stores.
    Duration(u16),
}

impl BacklightChange {
    /// Seconds in one HID++ `0x1982` fade-out unit. The feature stores
    /// durations in these units (`1..=0x05a0`).
    pub const DURATION_STEP_SECONDS: u16 = 5;

    /// Longest fade-out `0x1982` stores, in 5-second units.
    pub const MAX_DURATION_UNITS: u16 = 0x05a0;

    /// Longest fade-out `0x1982` stores: two hours.
    pub const MAX_DURATION_SECONDS: u16 = Self::MAX_DURATION_UNITS * Self::DURATION_STEP_SECONDS;

    /// The short timer Logi Options+ shows by default, in seconds.
    pub const SHORT_DURATION_SECONDS: [u16; 6] = [5, 10, 15, 20, 25, 30];

    /// Longer timers, up to [`Self::MAX_DURATION_SECONDS`]. Hidden until the
    /// panel's extended-duration switch is on.
    pub const EXTENDED_DURATION_SECONDS: [u16; 7] = [60, 120, 300, 600, 1_800, 3_600, 7_200];

    /// Whether `seconds` is a fade-out the protocol can store: a positive
    /// multiple of [`Self::DURATION_STEP_SECONDS`], at most two hours.
    #[must_use]
    pub const fn duration_offered(seconds: u16) -> bool {
        seconds >= Self::DURATION_STEP_SECONDS
            && seconds <= Self::MAX_DURATION_SECONDS
            && seconds.is_multiple_of(Self::DURATION_STEP_SECONDS)
    }

    /// Whether `seconds` is one of the steps `extended` is showing.
    #[must_use]
    pub const fn duration_step(seconds: u16, extended: bool) -> bool {
        Self::listed(seconds, &Self::SHORT_DURATION_SECONDS)
            || (extended && Self::listed(seconds, &Self::EXTENDED_DURATION_SECONDS))
    }

    const fn listed(seconds: u16, steps: &[u16]) -> bool {
        let mut index = 0;
        while index < steps.len() {
            if steps[index] == seconds {
                return true;
            }
            index += 1;
        }
        false
    }

    /// Fade-out units for a protocol duration, or `None` when `seconds` is not
    /// one the feature can store.
    #[must_use]
    pub const fn duration_units(seconds: u16) -> Option<u16> {
        if Self::duration_offered(seconds) {
            Some(seconds / Self::DURATION_STEP_SECONDS)
        } else {
            None
        }
    }

    /// Seconds a fade-out stored in [`Self::DURATION_STEP_SECONDS`] units
    /// lasts. Saturates; the feature's own maximum (`0x05a0` units) fits.
    #[must_use]
    pub const fn seconds_from_units(units: u16) -> u16 {
        units.saturating_mul(Self::DURATION_STEP_SECONDS)
    }
}

impl fmt::Display for BacklightChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on_off = |on: bool| if on { "on" } else { "off" };
        match *self {
            Self::Enabled(on) => write!(f, "backlight {}", on_off(on)),
            Self::Automatic(on) => write!(f, "automatic brightness {}", on_off(on)),
            Self::Level(level) => write!(f, "backlight level {level}"),
            Self::PowerSave(on) => write!(f, "battery saving {}", on_off(on)),
            Self::Duration(seconds) => write!(f, "backlight duration {seconds}s"),
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
                BacklightChange::Duration(seconds) => BacklightChange::duration_offered(seconds),
                BacklightChange::Enabled(_)
                | BacklightChange::Automatic(_)
                | BacklightChange::PowerSave(_) => true,
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

    /// The backlight this one becomes once the keyboard keeps `change`, as
    /// far as the change alone decides — what software can show before the
    /// keyboard reads back, and what a keyboard double answers.
    ///
    /// The firmware owns the rest and it is kept: a critical-battery shutoff
    /// stays in force, and a status the mode did not change — a saturated
    /// sensor, say — stands. Turning the backlight off always reads
    /// [`BacklightStatus::DisabledBySoftware`], the status the keyboard
    /// reports for it. `change` is not checked against [`Self::accepts`].
    #[must_use]
    pub fn changed(self, change: BacklightChange) -> Self {
        let mut next = self;
        match change {
            BacklightChange::Enabled(on) => next.enabled = on,
            BacklightChange::Automatic(true) => next.mode = BacklightMode::Automatic,
            // The level shown now is the one held, as the device write does.
            BacklightChange::Automatic(false) => next.mode = BacklightMode::PermanentManual,
            BacklightChange::Level(level) => {
                next.mode = BacklightMode::PermanentManual;
                next.current_level = level;
            }
            BacklightChange::PowerSave(on) => next.power_save = on,
            BacklightChange::Duration(seconds) => next.duration_secs = seconds,
        }
        next.status = if !next.enabled {
            BacklightStatus::DisabledBySoftware
        } else if self.status == BacklightStatus::DisabledByCriticalBattery
            || (self.enabled && next.mode == self.mode)
        {
            self.status
        } else {
            match next.mode {
                BacklightMode::Automatic => BacklightStatus::AlsAutomatic,
                BacklightMode::TemporaryManual => BacklightStatus::TemporaryManual,
                BacklightMode::PermanentManual => BacklightStatus::PermanentManual,
                BacklightMode::None => self.status,
            }
        };
        next
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
            duration_secs: 30,
            controls: BacklightControls {
                automatic: true,
                level: true,
                power_save: true,
                duration: true,
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
            BacklightChange::Duration(15),
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
    fn a_duration_must_be_a_protocol_step_through_two_hours() {
        for seconds in BacklightChange::SHORT_DURATION_SECONDS
            .into_iter()
            .chain(BacklightChange::EXTENDED_DURATION_SECONDS)
        {
            assert!(
                lit().accepts(BacklightChange::Duration(seconds)),
                "{seconds}"
            );
            assert!(BacklightChange::duration_step(seconds, seconds > 30));
            assert_eq!(BacklightChange::duration_units(seconds), Some(seconds / 5));
        }
        assert!(!lit().accepts(BacklightChange::Duration(0)));
        assert!(!lit().accepts(BacklightChange::Duration(7)));
        // 35 seconds is a legal fade-out, just not a short step.
        assert!(lit().accepts(BacklightChange::Duration(35)));
        assert!(!BacklightChange::duration_step(35, true));
        assert!(BacklightChange::duration_units(7).is_none());
        assert_eq!(
            BacklightChange::duration_units(BacklightChange::MAX_DURATION_SECONDS),
            Some(BacklightChange::MAX_DURATION_UNITS)
        );
        assert!(BacklightChange::duration_units(7_205).is_none());
        // The feature stores 5-second units. 6 units is the 30 second step.
        assert_eq!(BacklightChange::seconds_from_units(6), 30);
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
        assert_eq!(
            BacklightChange::Duration(15).to_string(),
            "backlight duration 15s"
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

    #[test]
    fn turning_the_backlight_off_reads_as_a_software_disable() {
        let off = lit().changed(BacklightChange::Enabled(false));
        assert!(!off.enabled);
        assert_eq!(off.status, BacklightStatus::DisabledBySoftware);
        assert_eq!(
            off.mode,
            BacklightMode::Automatic,
            "the mode is kept for later"
        );
    }

    #[test]
    fn turning_it_back_on_reports_the_kept_mode() {
        let off = lit().changed(BacklightChange::Enabled(false));
        assert_eq!(off.changed(BacklightChange::Enabled(true)), lit());
        let manual = BacklightState {
            mode: BacklightMode::PermanentManual,
            ..off
        };
        assert_eq!(
            manual.changed(BacklightChange::Enabled(true)).status,
            BacklightStatus::PermanentManual
        );
    }

    #[test]
    fn leaving_automatic_holds_the_level_shown() {
        let held = lit().changed(BacklightChange::Automatic(false));
        assert_eq!(held.mode, BacklightMode::PermanentManual);
        assert_eq!(held.status, BacklightStatus::PermanentManual);
        assert_eq!(held.current_level, lit().current_level);
        let back = held.changed(BacklightChange::Automatic(true));
        assert_eq!(back.mode, BacklightMode::Automatic);
        assert_eq!(back.status, BacklightStatus::AlsAutomatic);
    }

    #[test]
    fn a_level_leaves_automatic_mode() {
        let set = lit().changed(BacklightChange::Level(2));
        assert_eq!(set.current_level, 2);
        assert_eq!(set.mode, BacklightMode::PermanentManual);
        assert_eq!(set.status, BacklightStatus::PermanentManual);
    }

    #[test]
    fn a_duration_changes_only_the_timer() {
        let shorter = lit().changed(BacklightChange::Duration(15));
        assert_eq!(
            shorter,
            BacklightState {
                duration_secs: 15,
                ..lit()
            }
        );
        // Already on that step: nothing visible would change.
        assert_eq!(lit().changed(BacklightChange::Duration(30)), lit());
    }

    #[test]
    fn battery_saving_changes_nothing_else() {
        let saving_off = lit().changed(BacklightChange::PowerSave(false));
        assert_eq!(
            saving_off,
            BacklightState {
                power_save: false,
                ..lit()
            }
        );
    }

    #[test]
    fn firmware_owned_status_is_kept() {
        let saturated = BacklightState {
            status: BacklightStatus::AlsSaturated,
            ..lit()
        };
        assert_eq!(
            saturated.changed(BacklightChange::PowerSave(false)).status,
            BacklightStatus::AlsSaturated,
            "the sensor still reads bright"
        );
        let critical = BacklightState {
            status: BacklightStatus::DisabledByCriticalBattery,
            ..lit()
        };
        assert_eq!(
            critical.changed(BacklightChange::Level(3)).status,
            BacklightStatus::DisabledByCriticalBattery,
            "a level does not override the battery shutoff"
        );
        assert_eq!(
            critical.changed(BacklightChange::Enabled(false)).status,
            BacklightStatus::DisabledBySoftware
        );
    }
}
