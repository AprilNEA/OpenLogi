//! Keyboard Fn-lock: the persisted preference and the keyboard's own reading.
//!
//! Two facts, one authority each. The *setting* is `config.toml`'s per-device
//! `fn_lock`, which the agent writes on reload and re-applies on reconnect.
//! The *reading* is the swr-backed device query the device-read service owns,
//! showing what the keyboard holds right now — which can differ from the
//! setting after the user flips it from the keyboard with Fn+Esc.

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

    /// Persist `fn_lock` for the active keyboard and reload the agent, which
    /// writes it to the keyboard; then re-read it so the row shows what the
    /// keyboard took. No-op when no device is selected or it reports no
    /// Fn-lock control.
    pub fn commit_fn_lock(&mut self, fn_lock: bool) -> StateEvents {
        let events = self.for_current_device(StateEvent::DeviceConfigChanged);
        if !self.current_fn_lock_supported() {
            return events;
        }
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return events;
        };
        self.config.edit(|config| config.set_fn_lock(&key, fn_lock));
        if self.persist_and_reload("fn-lock") {
            // The agent's write is asynchronous; the query keeps the old
            // value on screen until the keyboard answers.
            let key = self.current_record().map(DeviceRecord::device_key);
            if let Some(key) = key {
                self.pointer.reads.refresh_fn_lock(&key);
            }
        }
        events
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
