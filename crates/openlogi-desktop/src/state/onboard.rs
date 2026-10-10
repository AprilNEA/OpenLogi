//! G-series onboard memory and report rate.

use std::time::Duration;

use gpui::App;
use openlogi_core::binding::GamingLayout;
use openlogi_core::config::OnboardMemory;
use openlogi_core::hid::{OnboardMode, OnboardState, ReportRate};

use super::events::StateEvents;
use super::load::{Load, OnboardLoad};
use super::{AppState, StateEvent};
use crate::state::devices::DeviceRecord;

impl AppState {
    #[must_use]
    pub fn current_onboard_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.onboard_profiles)
    }

    #[must_use]
    pub fn current_gaming_layout(&self) -> Option<&'static GamingLayout> {
        self.current_record()
            .and_then(|record| GamingLayout::for_model_key(&record.model_key))
    }

    #[must_use]
    pub fn current_onboard_load(&self) -> OnboardLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.onboard_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn current_onboard_memory(&self) -> Option<OnboardMemory> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.config
            .effective_onboard_memory(key, self.current_gaming_layout())
    }

    /// Who controls the mouse now, which is not always who config asked for:
    /// a host setup the mouse rejected leaves it on its own profile.
    #[must_use]
    pub fn current_onboard_active(&self) -> Option<OnboardMemory> {
        let reading = match self.current_onboard_load() {
            Load::Ready(reading) => Some(reading),
            _ => None,
        };
        active_memory(reading.as_deref(), self.current_onboard_memory())
    }

    #[must_use]
    pub fn current_report_rate_setting(&self) -> Option<ReportRate> {
        let key = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)?;
        self.config.report_rate(key)
    }

    pub fn commit_onboard_memory(&mut self, memory: OnboardMemory) -> StateEvents {
        let events = self.for_current_device(StateEvent::OnboardChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return events;
        };
        self.config
            .edit(|config| config.set_onboard_memory(&key, Some(memory)));
        self.persist_and_reload("onboard memory");
        events
    }

    pub fn commit_report_rate(&mut self, rate: ReportRate) -> StateEvents {
        let events = self.for_current_device(StateEvent::OnboardChanged);
        let Some(key) = self
            .current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .map(str::to_string)
        else {
            return events;
        };
        self.config
            .edit(|config| config.set_report_rate(&key, rate));
        self.persist_and_reload("report rate");
        events
    }

    /// Re-read the active G-series mouse now and once more after the agent has
    /// settled, for a change made outside the Pointer tab: a binding edit can
    /// move the mouse in or out of host mode.
    pub(crate) fn refresh_onboard(cx: &mut App) {
        let supported = Self::update(cx, |state, _| {
            let key = state
                .current_record()
                .filter(|_| state.current_onboard_supported())
                .map(DeviceRecord::device_key)?;
            state.pointer.reads.refresh_onboard(&key);
            Some(())
        });
        if supported.is_some() {
            Self::refresh_onboard_later(cx);
        }
    }

    /// Re-read the active mouse while the agent applies a change: early for
    /// the usual case, and again once it has had time to settle.
    pub(crate) fn refresh_onboard_later(cx: &mut App) {
        const WAITS: [Duration; 2] = [Duration::from_millis(700), Duration::from_millis(1300)];
        Self::update(cx, |state, cx| {
            let Some(key) = state.current_record().map(DeviceRecord::device_key) else {
                return;
            };
            cx.spawn(async move |state, cx| {
                for wait in WAITS {
                    cx.background_executor().timer(wait).await;
                    state
                        .update(cx, |state, _| {
                            state.pointer.reads.refresh_onboard(&key);
                            // A profile switch also changes the sensor DPI.
                            state.pointer.reads.retry_dpi(&key);
                        })
                        .ok();
                }
            })
            .detach();
        });
    }

    pub(super) fn load_current_onboard(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.onboard_profiles)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_onboard(key, route, self.ipc_sender(), cx);
    }
}

/// The mode the mouse reports, or `requested` until it has been read.
fn active_memory(
    reading: Option<&OnboardState>,
    requested: Option<OnboardMemory>,
) -> Option<OnboardMemory> {
    reading
        .map(|reading| match (reading.mode, reading.active_profile) {
            (OnboardMode::Onboard, Some(index)) => OnboardMemory::Profile(index),
            _ => OnboardMemory::Off,
        })
        .or(requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(mode: OnboardMode, active_profile: Option<u8>) -> OnboardState {
        OnboardState {
            mode,
            active_profile,
            profiles: Vec::new(),
            report_rate: None,
        }
    }

    /// Host mode was requested, but the mouse rejected it and is back on a
    /// profile: the card must not claim OpenLogi is in control.
    #[test]
    fn the_mouse_reading_wins_over_the_requested_mode() {
        let on_profile = reading(OnboardMode::Onboard, Some(5));
        assert_eq!(
            active_memory(Some(&on_profile), Some(OnboardMemory::Off)),
            Some(OnboardMemory::Profile(5))
        );
        let in_host = reading(OnboardMode::Host, None);
        assert_eq!(
            active_memory(Some(&in_host), Some(OnboardMemory::Profile(1))),
            Some(OnboardMemory::Off)
        );
    }

    #[test]
    fn the_requested_mode_stands_in_until_the_mouse_is_read() {
        assert_eq!(
            active_memory(None, Some(OnboardMemory::Profile(2))),
            Some(OnboardMemory::Profile(2))
        );
        assert_eq!(active_memory(None, None), None);
    }
}
