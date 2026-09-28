//! A keyboard's Fn lock: read from the device (Fn+Esc changes it behind
//! OpenLogi's back), written through `config.toml` so the agent re-applies it
//! on reconnect. The read is an swr-backed query owned by the device-read
//! service.

use gpui::{App, Context};
use openlogi_core::device::DeviceKind;
use tracing::debug;

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::FnLockLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Subscribe to the selected keyboard's Fn lock. Other device kinds are
    /// never asked. Runs on every agent snapshot, so an existing subscription
    /// is left alone; [`Self::revalidate_current_fn_lock`] re-reads it.
    pub(super) fn load_current_fn_lock(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| record.kind == DeviceKind::Keyboard)
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_fn_lock(key, route, self.ipc_sender(), cx);
    }

    /// Re-read the selected keyboard's Fn lock, e.g. when the Keys tab opens:
    /// Fn+Esc on the keyboard may have changed it since the last read.
    pub(crate) fn revalidate_current_fn_lock(&mut self) {
        let key = self.current_record().map(DeviceRecord::device_key);
        debug!(?key, "Keys tab opened");
        if let Some(key) = key {
            self.pointer.reads.revalidate_fn_lock(&key);
        }
    }

    /// The selected keyboard's Fn-lock read, or `None` when no keyboard is
    /// selected or it has not been asked yet.
    #[must_use]
    pub fn current_fn_lock(&self) -> Option<&FnLockLoad> {
        self.current_record()
            .and_then(|record| self.pointer.reads.fn_lock_load(&record.device_key()))
    }

    /// Adopt a Fn-lock read into config when it disagrees with a saved
    /// setting: Fn+Esc on the keyboard changed it, and the agent re-applies
    /// the saved value on reconnect, so a stale one would undo that press.
    /// A keyboard nobody set from OpenLogi stays unset.
    pub(crate) fn apply_fn_lock_read(&mut self, key: &DeviceKey) {
        if !self.is_current_device(key) {
            return;
        }
        let Some(FnLockLoad::Ready(on)) = self.pointer.reads.fn_lock_load(key) else {
            return;
        };
        let on = **on;
        let Some(config_key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return;
        };
        if self
            .config
            .fn_lock(&config_key)
            .is_some_and(|saved| saved != on)
        {
            self.config
                .edit(|config| config.set_fn_lock(&config_key, on));
            self.persist_and_reload("Fn lock from the keyboard");
        }
    }

    /// Set the selected keyboard's Fn lock from the Keys tab switch.
    pub(crate) fn update_fn_lock(cx: &mut App, on: bool) {
        Self::apply(cx, |state| state.commit_fn_lock(on));
    }

    /// Set the selected keyboard's Fn lock: persist it, have the agent write
    /// it, and show it at once rather than waiting for a re-read. No-op
    /// unless a keyboard with a persistent config key is selected.
    pub fn commit_fn_lock(&mut self, on: bool) -> StateEvents {
        let Some(record) = self
            .current_record()
            .filter(|record| record.kind == DeviceKind::Keyboard)
        else {
            return StateEvents::none();
        };
        let device_key = record.device_key();
        let Some(config_key) = record.persistent_config_key().map(str::to_string) else {
            debug!("no persistent device key — Fn-lock change ignored");
            return StateEvents::none();
        };
        self.config
            .edit(|config| config.set_fn_lock(&config_key, on));
        if self.persist_and_reload("Fn lock") {
            self.pointer.reads.set_fn_lock_ready(&device_key, on);
        }
        StateEvent::FnLockChanged(device_key).into()
    }
}
