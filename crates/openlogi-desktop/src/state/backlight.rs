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

use openlogi_core::hid::{BacklightChange, BacklightState, DeviceRoute, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::BacklightLoad;
use super::{AppState, StateEvent};

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
    /// moved on.
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
        self.pointer
            .reads
            .set_backlight_ready(&device_key, &route, next);
        self.send_ipc(crate::services::ipc::SetBacklight {
            route,
            change,
            key: device_key,
        });
        events
    }

    /// The keyboard's answer to a [`Self::commit_backlight`] write: show the
    /// read-back, or — when the keyboard refused or could not be reached —
    /// ask it again so the predicted value does not stand in for its own.
    ///
    /// `route` is the connection the write went out on. Only a read still
    /// subscribed on `route` takes the answer; once the keyboard has moved,
    /// the new connection's own read stands.
    pub fn apply_backlight_written(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        result: Result<BacklightState, WriteError>,
    ) -> StateEvents {
        let landed = match result {
            Ok(state) => self.pointer.reads.set_backlight_ready(key, route, state),
            Err(error) => {
                let refreshed = self.pointer.reads.refresh_backlight(key, route);
                if refreshed {
                    warn!(%error, key = %key, "backlight write did not land — re-reading the keyboard");
                }
                refreshed
            }
        };
        if !landed {
            debug!(key = %key, "backlight answer for a connection no read is on — dropping it");
            return StateEvents::none();
        }
        StateEvent::BacklightChanged(key.clone()).into()
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
