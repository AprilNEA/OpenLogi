//! Keyboard Fn-lock: the persisted preference and the keyboard's own reading.
//!
//! Two facts, one authority each. The *setting* is `config.toml`'s per-device
//! `fn_lock`, which the agent re-applies when the keyboard reconnects. The
//! *reading* is the swr-backed device query the device-read service owns,
//! showing what the keyboard holds right now — which can differ from the
//! setting after the user flips it from the keyboard with Fn+Esc.
//!
//! A toggle flip persists the setting and writes the keyboard in one request
//! whose answer is the state the keyboard echoed, so the reading is never a
//! guess about a write still in flight: the toggle shows the asked-for value
//! at once, then the echo replaces it, or a refusal sends the query back to
//! the keyboard for the truth.

use openlogi_core::hid::{FnLockState, WriteError};
use tracing::warn;

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::FnLockLoad;
use super::{AppState, StateEvent};
use crate::state::devices::DeviceRecord;

impl AppState {
    /// Whether the active device reports an Fn-lock control (HID++ `0x40a2`
    /// or `0x40a3`).
    #[must_use]
    pub fn current_fn_lock_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.fn_lock)
    }

    /// The persisted Fn-lock preference for the active device, or `None` when
    /// the user never set one (the keyboard keeps its own state).
    #[must_use]
    pub fn current_fn_lock_setting(&self) -> Option<bool> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.fn_lock(key))
    }

    /// What is known of the active keyboard's own Fn-lock state.
    #[must_use]
    pub fn current_fn_lock_load(&self) -> FnLockLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.fn_lock_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// The Fn-lock state the toggle should show: the keyboard's own reading
    /// when it has landed, else the persisted preference, else off.
    #[must_use]
    pub fn current_fn_lock_shown(&self) -> bool {
        match self.current_fn_lock_load() {
            FnLockLoad::Ready(state) => state.fn_lock,
            FnLockLoad::Unknown
            | FnLockLoad::Loading
            | FnLockLoad::Failed(_)
            | FnLockLoad::Unsupported(_) => self.current_fn_lock_setting().unwrap_or(false),
        }
    }

    /// Persist `fn_lock` for the active keyboard and write it to the keyboard
    /// through the agent. The toggle shows `fn_lock` at once; the keyboard's
    /// echo arrives as [`Self::apply_fn_lock_written`]. No-op when no device
    /// is selected or it reports no Fn-lock control.
    pub fn commit_fn_lock(&mut self, fn_lock: bool) -> StateEvents {
        let events = self.for_current_device(StateEvent::FnLockChanged);
        if !self.current_fn_lock_supported() {
            return events;
        }
        let Some(record) = self.current_record() else {
            return events;
        };
        let device_key = record.device_key();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        if let Some(persistent_key) = persistent_key {
            self.config
                .edit(|config| config.set_fn_lock(&persistent_key, fn_lock));
            if !self.persist_and_reload("fn-lock") {
                return events;
            }
        }
        let Some(route) = route else {
            return events;
        };
        let default_fn_lock = match self.current_fn_lock_load() {
            FnLockLoad::Ready(state) => state.default_fn_lock,
            FnLockLoad::Unknown
            | FnLockLoad::Loading
            | FnLockLoad::Failed(_)
            | FnLockLoad::Unsupported(_) => false,
        };
        self.pointer.reads.set_fn_lock_ready(
            &device_key,
            FnLockState {
                fn_lock,
                default_fn_lock,
            },
        );
        self.send_ipc(crate::services::ipc::SetFnLock {
            route,
            fn_lock,
            key: device_key,
        });
        events
    }

    /// The keyboard's answer to a [`Self::commit_fn_lock`] write: show the
    /// echoed state, or — when the keyboard refused or could not be reached —
    /// ask it again so the optimistic value does not stand in for its own.
    pub fn apply_fn_lock_written(
        &mut self,
        key: &DeviceKey,
        result: Result<FnLockState, WriteError>,
    ) -> StateEvents {
        match result {
            Ok(state) => self.pointer.reads.set_fn_lock_ready(key, state),
            Err(error) => {
                warn!(%error, key = %key, "Fn-lock write did not land — re-reading the keyboard");
                self.pointer.reads.refresh_fn_lock(key);
            }
        }
        StateEvent::FnLockChanged(key.clone()).into()
    }

    pub(super) fn load_current_fn_lock(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.fn_lock)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_fn_lock(key, route, self.ipc_sender(), cx);
    }
}
