//! Keyboard backlight: the keyboard's own reading, changed one control at a
//! time.
//!
//! Unlike the report rate there is no setting in `config.toml`. The keyboard
//! keeps its backlight in its own memory and changes it by itself — the
//! backlight keys, the light sensor, a low battery — so the swr-backed device
//! query the device-read service owns is the only record, and the panel only
//! offers the changes that reading says the keyboard accepts.
//!
//! A change writes the keyboard in one request whose answer is its read-back:
//! the change shows at once, as [`BacklightState::changed`] predicts it, then
//! the read-back replaces it, or a refusal sends the query back to the
//! keyboard for the truth. The answer names the connection it was read on and
//! is dropped once the keyboard has moved to another one.
//!
//! Changes made in quick succession are each written, in order. Until a
//! change is answered it stays applied over whatever reading comes in, so the
//! answer to an earlier click cannot hide a later one (see
//! `DeviceReads::begin_backlight_write`).

use openlogi_core::hid::{BacklightChange, BacklightState, DeviceRoute, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::BacklightLoad;
use super::{AppState, StateEvent};

/// Which timer steps the Device tab's backlight card shows — a disclosure,
/// not a keyboard setting: the keyboard keeps whatever duration was last
/// written, and switching shows or hides steps without writing anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DurationSteps {
    /// The short timers Logi Options+ offers by default.
    #[default]
    Short,
    /// The short timers plus the longer steps, up to the two hours `0x1982`
    /// stores.
    Extended,
}

/// The timer steps the Device tab shows without the extended switch: the
/// short timers Logi Options+ offers by default.
pub(crate) const SHORT_DURATION_SECONDS: &[u16] = &[5, 10, 15, 20, 25, 30];

/// The longer timers added when the extended switch is on, up to the two
/// hours `0x1982` stores.
pub(crate) const EXTENDED_DURATION_SECONDS: &[u16] = &[60, 120, 300, 600, 1_800, 3_600, 7_200];

impl DurationSteps {
    /// The step tables this choice shows, in display order; the card draws
    /// one pill row per table.
    #[must_use]
    pub const fn steps(self) -> &'static [&'static [u16]] {
        match self {
            Self::Short => &[SHORT_DURATION_SECONDS],
            Self::Extended => &[SHORT_DURATION_SECONDS, EXTENDED_DURATION_SECONDS],
        }
    }

    /// Whether `seconds` is one of the steps this choice shows.
    #[must_use]
    pub const fn contains(self, seconds: u16) -> bool {
        match self {
            Self::Short => listed(seconds, SHORT_DURATION_SECONDS),
            Self::Extended => {
                listed(seconds, SHORT_DURATION_SECONDS)
                    || listed(seconds, EXTENDED_DURATION_SECONDS)
            }
        }
    }
}

/// Whether `seconds` is one of `steps`.
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

impl AppState {
    /// Whether the active device reports a backlight control (HID++
    /// `0x1982`).
    #[must_use]
    pub fn current_backlight_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.backlight)
    }

    /// What is known of the active keyboard's backlight.
    #[must_use]
    pub fn current_backlight_load(&self) -> BacklightLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.backlight_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// Write `change` to the active keyboard through the agent. The panel
    /// shows the changed backlight at once; the keyboard's read-back arrives
    /// as [`Self::apply_backlight_written`]. No-op when no device is selected,
    /// it reports no backlight control, or [`change_target`] finds no reading
    /// that accepts `change` or that `change` would alter — the panel only
    /// offers accepted changes, so those are clicks from before the reading
    /// moved on — or the reading is still the keyboard's previous connection.
    pub fn commit_backlight(&mut self, change: BacklightChange) -> StateEvents {
        let events = self.for_current_device(StateEvent::BacklightChanged);
        if !self.current_backlight_supported() {
            return events;
        }
        let load = self.current_backlight_load();
        let Some(next) = change_target(&load, change) else {
            debug!(%change, "the keyboard's last reading does not take this change — ignoring it");
            return events;
        };
        let Some((device_key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return events;
        };
        let Some(write) =
            self.pointer
                .reads
                .begin_backlight_write(&device_key, &route, change, next)
        else {
            debug!(%change, "the backlight reading is not on the keyboard's connection yet — ignoring the change");
            return events;
        };
        self.send_ipc(crate::services::ipc::SetBacklight {
            route,
            change,
            key: device_key,
            write,
        });
        events
    }

    /// The keyboard's answer to the [`Self::commit_backlight`] write numbered
    /// `write`: show the read-back, with any change made since still applied,
    /// or — when the keyboard refused or could not be reached — ask it again
    /// so the predicted value does not stand in for its own.
    ///
    /// `route` is the connection the write went out on. Only a read still
    /// subscribed on `route` takes the answer; once the keyboard has moved,
    /// the new connection's own read stands. An answer arriving after a
    /// newer change's answer is dropped too.
    pub fn apply_backlight_written(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        write: u64,
        result: Result<BacklightState, WriteError>,
    ) -> StateEvents {
        let (read_back, refusal) = match result {
            Ok(state) => (Some(state), None),
            Err(error) => (None, Some(error)),
        };
        if !self
            .pointer
            .reads
            .finish_backlight_write(key, route, write, read_back)
        {
            debug!(key = %key, write, "backlight answer superseded, or for a connection no read is on — dropping it");
            return StateEvents::none();
        }
        if let Some(error) = refusal {
            warn!(%error, key = %key, "backlight write did not land — re-reading the keyboard");
        }
        StateEvent::BacklightChanged(key.clone()).into()
    }

    /// Which timer steps the backlight card is showing.
    #[must_use]
    pub fn duration_steps(&self) -> DurationSteps {
        self.duration_steps
    }

    /// Show or hide timers past 30 seconds. Writes nothing to the keyboard.
    pub fn set_duration_steps(&mut self, steps: DurationSteps) -> StateEvents {
        self.duration_steps = steps;
        self.for_current_device(StateEvent::BacklightChanged)
    }

    /// Re-run `key`'s exhausted backlight read — the "click to retry"
    /// affordance on a [`BacklightLoad::Failed`] keyboard.
    pub(crate) fn retry_backlight_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_backlight(key);
        StateEvent::BacklightChanged(key.clone()).into()
    }

    pub(super) fn load_current_backlight(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.backlight)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_backlight(key, route, self.ipc_sender(), cx);
    }
}

/// The backlight a `change` is shown as while it is written: the keyboard's
/// last read-back with `change` applied, when there is a read-back, it
/// accepts `change`, and `change` alters it.
pub(super) fn change_target(
    load: &BacklightLoad,
    change: BacklightChange,
) -> Option<BacklightState> {
    match load {
        BacklightLoad::Ready(state) if state.accepts(change) => {
            Some(state.changed(change)).filter(|next| next != &**state)
        }
        BacklightLoad::Ready(_)
        | BacklightLoad::Unknown
        | BacklightLoad::Loading
        | BacklightLoad::Failed(_)
        | BacklightLoad::Unsupported(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::BacklightDuration;

    use super::{DurationSteps, EXTENDED_DURATION_SECONDS, SHORT_DURATION_SECONDS};

    #[test]
    fn every_step_the_panel_shows_is_a_valid_timer() {
        // The panel converts a step the user clicks with `expect`, which
        // stands only while every listed step is one the keyboard can store.
        for seconds in SHORT_DURATION_SECONDS
            .iter()
            .chain(EXTENDED_DURATION_SECONDS)
            .copied()
        {
            BacklightDuration::try_from(seconds)
                .expect("a panel step is within the two hours `0x1982` stores");
        }
    }

    #[test]
    fn contains_agrees_with_the_tables_the_choice_shows() {
        for seconds in 0..=BacklightDuration::MAX_SECONDS {
            assert_eq!(
                SHORT_DURATION_SECONDS.contains(&seconds),
                DurationSteps::Short.contains(seconds),
                "{seconds}: Short contains disagrees with its table"
            );
            let shown = SHORT_DURATION_SECONDS
                .iter()
                .chain(EXTENDED_DURATION_SECONDS)
                .any(|&step| step == seconds);
            assert_eq!(
                shown,
                DurationSteps::Extended.contains(seconds),
                "{seconds}: Extended contains disagrees with its tables"
            );
        }
    }
}
