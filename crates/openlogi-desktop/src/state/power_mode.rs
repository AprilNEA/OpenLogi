//! Power-mode (HID++ `0x8090`) reads and optimistic writes.
//!
//! Deliberately simpler than the SmartShift sibling: there is no config
//! persistence (the device keeps the mode across power cycles itself) and no
//! write-status banner. A write is sent, cached optimistically, and followed
//! by a confirming re-read that replaces the cache with whatever the device
//! really holds — a failed write becomes visible when the toggle flips back.

use gpui::{App, Context};
use openlogi_core::hid::{PowerMode, PowerModeState};
use tracing::debug;

use super::AppState;
use super::StateEvent;
use super::device_key::DeviceKey;
use super::load::PowerModeLoad;

impl AppState {
    /// Start any pending power-mode read for the selected device.
    pub(super) fn load_current_power_mode(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_power_mode(key, route, self.ipc_sender(), cx);
    }

    /// Re-arm a failed power-mode read from the panel's retry affordance.
    pub(crate) fn retry_power_mode_read(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            state.pointer.reads.retry_power_mode(&key);
            cx.emit(StateEvent::PowerModeChanged(key));
        });
    }

    /// The active device's resolved power mode, if the read succeeded.
    #[must_use]
    pub fn current_power_mode_ready(&self) -> Option<PowerModeState> {
        self.current_record()
            .and_then(|record| self.pointer.reads.power_mode_load(&record.device_key()))
            .and_then(|load| match load {
                PowerModeLoad::Ready(state) => Some(**state),
                PowerModeLoad::Unknown
                | PowerModeLoad::Loading
                | PowerModeLoad::Failed(_)
                | PowerModeLoad::Unsupported(_) => None,
            })
    }

    /// The load state backing the panel for `key`.
    pub(crate) fn power_mode_status_for(&self, key: &DeviceKey) -> PowerModeLoad {
        self.pointer.reads.power_mode_status(key)
    }

    /// Write `mode` to the active device, cache it optimistically, and queue a
    /// confirming re-read that replaces the optimistic value with whatever the
    /// device reports. No-op when no device is selected, it is offline, or its
    /// power mode never resolved.
    pub(crate) fn update_power_mode(cx: &mut App, mode: PowerMode) {
        Self::update(cx, |state, cx| {
            let Some((key, route)) = state
                .current_record()
                .and_then(|record| Some((record.device_key(), record.route.clone()?)))
            else {
                debug!("no reachable device — power-mode change ignored");
                return;
            };
            let Some(current) = state.current_power_mode_ready() else {
                debug!("power mode unresolved — change ignored");
                return;
            };
            state.send_ipc(crate::services::ipc::Command::SetPowerMode(
                route.clone(),
                mode,
            ));
            let expected = PowerModeState { mode, ..current };
            state.pointer.reads.set_power_mode_ready(&key, expected);
            state
                .pointer
                .reads
                .confirm_power_mode(key.clone(), route, state.ipc_sender(), cx);
            cx.emit(StateEvent::PowerModeChanged(key));
        });
    }
}
