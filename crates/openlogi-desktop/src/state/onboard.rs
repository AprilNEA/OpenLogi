//! G-series onboard memory and report rate.

use openlogi_core::binding::GamingLayout;
use openlogi_core::config::OnboardMemory;
use openlogi_core::hid::ReportRate;

use super::events::StateEvents;
use super::load::OnboardLoad;
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
