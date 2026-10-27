//! HID++ keyboard-backlight mode, status, read-back state, and changes.
//!
//! These types describe the semantic state of HID++ `Backlight` feature
//! `0x1982`. The protocol conversion and transport operations that produce
//! them remain in `openlogi-device`.

use std::fmt;

use nutype::nutype;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct as _;
use serde::{Deserialize, Serialize};
use thiserror::Error as ThisError;

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
    /// How long the keys stay lit after typing stops.
    ///
    /// From the keyboard's hands-out fade-out, which Logi Options+ shows as
    /// the backlight timer. `None` when the keyboard does not offer a
    /// duration or reports none. A value outside the steps the panel is
    /// showing is still reported as stored — a longer timer stays visible
    /// instead of being snapped to 30 seconds.
    ///
    /// `serde(default)` only keeps recorded JSON fixtures that predate the
    /// field loading; the IPC encoding always carries it.
    #[serde(default)]
    pub duration: Option<BacklightDuration>,
}

bitflags::bitflags! {
    /// What software may change on a keyboard backlight beyond turning it on
    /// or off, derived from the `0x1982` capability bits and feature version.
    ///
    /// A bit set: the offers are independent — the `0x1982` layout reports
    /// some without others, and a named-field struct of bools is exactly the
    /// shape `clippy::struct_excessive_bools` hears as an untyped state
    /// machine. Crosses the agent↔GUI IPC inside [`BacklightState`] as a
    /// single byte — changes to that layout require a `PROTOCOL_VERSION`
    /// bump (guarded by `openlogi-ipc/tests/wire_format.rs`) — while
    /// recorded profiles write the released three-field object left as
    /// `automatic`/`level`/`power_save`, plus `duration`, and read that
    /// shape from released builds.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct BacklightControls: u8 {
        /// Software can switch between the ambient-light sensor choosing the
        /// level and holding one (permanent-manual mode): the keyboard
        /// supports both.
        const AUTOMATIC = 1 << 0;
        /// Software can pick the level: permanent-manual mode, `0x1982`
        /// version 3 or later, and more than one level.
        const LEVEL = 1 << 1;
        /// The firmware can turn the backlight off on a low battery.
        const POWER_SAVE = 1 << 2;
        /// Software can set how long the keys stay lit after typing stops:
        /// `0x1982` version 3 or later, which is the layout those durations
        /// are written in.
        const DURATION = 1 << 3;
    }
}

impl Default for BacklightControls {
    fn default() -> Self {
        Self::empty()
    }
}

impl BacklightControls {
    /// Whether `change` is one this keyboard accepts, ignoring the level
    /// range and the duration range: [`BacklightChange::Level`] also needs a
    /// level below [`BacklightState::nb_levels`], and
    /// [`BacklightChange::Duration`] a validated timer.
    #[must_use]
    pub const fn allow(self, change: BacklightChange) -> bool {
        match change {
            BacklightChange::Enabled(_) => true,
            BacklightChange::Automatic(_) => self.contains(Self::AUTOMATIC),
            BacklightChange::Level(_) => self.contains(Self::LEVEL),
            BacklightChange::PowerSave(_) => self.contains(Self::POWER_SAVE),
            BacklightChange::Duration(_) => self.contains(Self::DURATION),
        }
    }
}

/// Field names of the object shape recorded profiles carry: the released
/// three fields, `duration` new here. Records from released builds have no
/// `duration` key.
const CONTROLS_FIELDS: [&str; 4] = ["automatic", "level", "power_save", "duration"];

impl Serialize for BacklightControls {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            // The released three-field object, written back through this
            // build's `duration` bit; a re-recorded profile is a release
            // record plus its `duration` key.
            let mut object = serializer.serialize_struct("BacklightControls", 4)?;
            object.serialize_field("automatic", &self.contains(Self::AUTOMATIC))?;
            object.serialize_field("level", &self.contains(Self::LEVEL))?;
            object.serialize_field("power_save", &self.contains(Self::POWER_SAVE))?;
            object.serialize_field("duration", &self.contains(Self::DURATION))?;
            object.end()
        } else {
            // The IPC payload carries the set as one byte.
            self.bits().serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for BacklightControls {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            deserializer.deserialize_any(ControlsVisitor::default())
        } else {
            u8::deserialize(deserializer).map(Self::from_bits_retain)
        }
    }
}

/// Reads either recorded profile shape: the released object or — so a future
/// schema change cannot strand a reader — the set as its bit number.
#[derive(Default)]
struct ControlsVisitor {
    controls: BacklightControls,
}

impl<'de> Visitor<'de> for ControlsVisitor {
    type Value = BacklightControls;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "the recorded backlight controls object (three released fields plus \
             `duration`) or the controls as a bit number",
        )
    }

    /// The recorded object shape: missing bits are `false`, matching the
    /// `#[serde(default)]` the removed fields carried.
    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut value = Self::default();
        while let Some(key) = map.next_key::<String>()? {
            let bit = match key.as_str() {
                "automatic" => BacklightControls::AUTOMATIC,
                "level" => BacklightControls::LEVEL,
                "power_save" => BacklightControls::POWER_SAVE,
                "duration" => BacklightControls::DURATION,
                other => return Err(de::Error::unknown_field(other, &CONTROLS_FIELDS)),
            };
            if map.next_value()? {
                value.controls |= bit;
            }
        }
        Ok(value.controls)
    }

    /// A future build may record the set as its bit number.
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        let bits = u8::try_from(value).map_err(|_| {
            de::Error::invalid_value(
                de::Unexpected::Unsigned(value),
                &"a backlight controls bit set within one byte",
            )
        })?;
        Ok(BacklightControls::from_bits_retain(bits))
    }
}

/// One keyboard fade-out the `0x1982` timer can store: a positive multiple
/// of five seconds, at most two hours — one unit through `0x05a0`.
///
/// Validated rather than a bare `u16`, so no calling layer re-checks the
/// 5-second rule and [`BacklightChange::Duration`] cannot carry a timer the
/// write layer would have to reject. The wire and the TOML still carry plain
/// seconds; serde goes through the same validation.
#[nutype(
    validate(with = validate_fade_out, error = BacklightDurationError),
    derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        TryFrom,
        Into,
        Serialize,
        Deserialize
    )
)]
pub struct BacklightDuration(u16);

impl BacklightDuration {
    /// Seconds in one HID++ `0x1982` fade-out unit.
    pub const UNIT_SECONDS: u16 = 5;

    /// Longest fade-out `0x1982` stores, in units.
    pub const MAX_UNITS: u16 = 0x05a0;

    /// Longest fade-out `0x1982` stores: two hours.
    pub const MAX_SECONDS: u16 = Self::MAX_UNITS * Self::UNIT_SECONDS;

    /// The duration in seconds.
    #[must_use]
    pub fn seconds(self) -> u16 {
        self.into_inner()
    }

    /// The duration in the feature's stored units, `1..=[Self::MAX_UNITS]`.
    #[must_use]
    pub fn units(self) -> u16 {
        self.into_inner() / Self::UNIT_SECONDS
    }

    /// The fade-out stored in `units`, or `None` when the device reported
    /// none (`0`, what a keyboard without the timer writes) or a length
    /// beyond the two hours this feature version stores.
    #[must_use]
    pub fn from_units(units: u16) -> Option<Self> {
        Self::try_new(units.checked_mul(Self::UNIT_SECONDS)?).ok()
    }
}

/// Why a value is not a [`BacklightDuration`].
#[derive(Clone, Debug, PartialEq, Eq, ThisError)]
pub enum BacklightDurationError {
    /// No positive multiple of the 5-second unit: `0`.
    #[error("the fade-out must be a positive multiple of five seconds, {0} was given")]
    BelowOneUnit(u16),
    /// A value between two 5-second steps.
    #[error("the fade-out must be a multiple of five seconds, {0} was given")]
    BetweenSteps(u16),
    /// A fade-out longer than the two hours `0x1982` stores.
    #[error("the fade-out cannot exceed two hours (7200 seconds), {0} was given")]
    BeyondTwoHours(u16),
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "nutype hands the validator a reference to the inner value"
)]
fn validate_fade_out(seconds: &u16) -> Result<(), BacklightDurationError> {
    let seconds = *seconds;
    if seconds < BacklightDuration::UNIT_SECONDS {
        Err(BacklightDurationError::BelowOneUnit(seconds))
    } else if !seconds.is_multiple_of(BacklightDuration::UNIT_SECONDS) {
        Err(BacklightDurationError::BetweenSteps(seconds))
    } else if seconds > BacklightDuration::MAX_SECONDS {
        Err(BacklightDurationError::BeyondTwoHours(seconds))
    } else {
        Ok(())
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
    /// Keep the keys lit this long after typing stops.
    Duration(BacklightDuration),
}

impl fmt::Display for BacklightChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on_off = |on: bool| if on { "on" } else { "off" };
        match *self {
            Self::Enabled(on) => write!(f, "backlight {}", on_off(on)),
            Self::Automatic(on) => write!(f, "automatic brightness {}", on_off(on)),
            Self::Level(level) => write!(f, "backlight level {level}"),
            Self::PowerSave(on) => write!(f, "battery saving {}", on_off(on)),
            Self::Duration(seconds) => write!(f, "backlight duration {}s", seconds.seconds()),
        }
    }
}

/// Highest selectable level when the device reports a count including zero.
#[must_use]
pub const fn max_backlight_level(nb_levels: u8) -> u8 {
    nb_levels.saturating_sub(1)
}

impl BacklightState {
    /// Whether the keyboard accepts `change` right now: the control exists
    /// and, for a level, the level is in range.
    #[must_use]
    pub fn accepts(self, change: BacklightChange) -> bool {
        self.controls.allow(change)
            // A duration is valid by construction; only a level needs an
            // in-range check against this keyboard.
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
            BacklightChange::Duration(seconds) => next.duration = Some(seconds),
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

    #[test]
    fn level_count_includes_the_off_position() {
        assert_eq!(max_backlight_level(8), 7);
        assert_eq!(max_backlight_level(1), 0);
        assert_eq!(max_backlight_level(0), 0);
    }

    /// A test duration that is one of the panel steps; the type itself
    /// guarantees legality.
    fn duration(seconds: u16) -> BacklightDuration {
        BacklightDuration::try_from(seconds).expect("a listed step is a valid duration")
    }

    fn lit() -> BacklightState {
        BacklightState {
            enabled: true,
            mode: BacklightMode::Automatic,
            status: BacklightStatus::AlsAutomatic,
            current_level: 4,
            nb_levels: 8,
            power_save: true,
            duration: Some(duration(30)),
            controls: BacklightControls::AUTOMATIC
                | BacklightControls::LEVEL
                | BacklightControls::POWER_SAVE
                | BacklightControls::DURATION,
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
            BacklightChange::Duration(duration(15)),
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
    fn a_duration_is_validated_at_construction_not_at_use() {
        // 35 seconds is a legal fade-out, just not a short step.
        assert!(lit().accepts(BacklightChange::Duration(duration(35))));
        let duration = BacklightDuration::try_from(30).expect("30 is a step");
        assert_eq!(duration.seconds(), 30);
        assert_eq!(
            duration.units(),
            6,
            "the feature stores 5-second units; 6 units is the 30 second step"
        );
        assert_eq!(BacklightDuration::from_units(6), Some(duration));
        assert_eq!(
            BacklightDuration::from_units(BacklightDuration::MAX_UNITS),
            BacklightDuration::try_from(BacklightDuration::MAX_SECONDS).ok()
        );
        // The device may report it has no timer written.
        assert_eq!(BacklightDuration::from_units(0), None);
        // Each rejection names exactly what a caller typed wrong.
        assert_eq!(
            BacklightDuration::try_from(0).unwrap_err(),
            BacklightDurationError::BelowOneUnit(0)
        );
        assert_eq!(
            BacklightDuration::try_from(7).unwrap_err(),
            BacklightDurationError::BetweenSteps(7)
        );
        assert_eq!(
            BacklightDuration::try_from(7_205).unwrap_err(),
            BacklightDurationError::BeyondTwoHours(7_205)
        );
        assert_eq!(
            BacklightDuration::try_from(7_200).err(),
            None,
            "two hours is the longest the feature stores"
        );
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
            BacklightChange::Duration(duration(15)).to_string(),
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
        let shorter = lit().changed(BacklightChange::Duration(duration(15)));
        assert_eq!(
            shorter,
            BacklightState {
                duration: Some(duration(15)),
                ..lit()
            }
        );
        // Already on that step: nothing visible would change.
        assert_eq!(
            lit().changed(BacklightChange::Duration(duration(30))),
            lit()
        );
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
