//! Report-rate reads and live writes. Capability discovery is an swr-backed
//! query owned by the device-read service.

use gpui::{App, Context};
use openlogi_core::hid::{ReportRateCapabilities, ReportRateHz};
use tracing::debug;

use super::device_key::DeviceKey;
use super::load::ReportRateStatus;
use super::{AppState, StateEvent};

impl AppState {
    pub(super) fn load_current_report_rate(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_report_rate(key.clone(), route, self.ipc_sender(), cx);
        self.apply_report_rate_read(&key);
    }

    pub(crate) fn retry_report_rate_read(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            state.pointer.reads.retry_report_rate(&key);
            cx.emit(StateEvent::ReportRateChanged(key));
        });
    }

    #[must_use]
    pub(crate) fn report_rate_for_current(&self) -> ReportRateHz {
        self.current_record()
            .and_then(|record| self.pointer.reads.report_rate_load(&record.device_key()))
            .and_then(|status| match status {
                ReportRateStatus::Ready(info) => Some(info.current),
                _ => None,
            })
            .unwrap_or(ReportRateHz::new(1000))
    }

    pub(crate) fn apply_report_rate_read(&mut self, key: &DeviceKey) {
        if self
            .current_record()
            .is_none_or(|record| record.device_key() != *key)
        {
            return;
        }
        if let Some(ReportRateStatus::Ready(info)) = self.pointer.reads.report_rate_load(key) {
            self.pointer.report_rate = info.current;
        }
    }

    #[must_use]
    pub fn active_report_rate_capabilities(&self) -> Option<&ReportRateCapabilities> {
        self.current_record()
            .and_then(|record| self.pointer.reads.report_rate_load(&record.device_key()))
            .and_then(|status| match status {
                ReportRateStatus::Ready(info) => Some(&info.capabilities),
                ReportRateStatus::Unknown
                | ReportRateStatus::Loading
                | ReportRateStatus::Failed(_)
                | ReportRateStatus::Unsupported(_) => None,
            })
    }

    /// Snap `rate` to the active device's supported list when known.
    #[must_use]
    pub fn normalize_active_report_rate(&self, rate: ReportRateHz) -> ReportRateHz {
        self.active_report_rate_capabilities()
            .map_or(rate, |caps| caps.nearest(rate))
    }

    pub fn commit_report_rate(&mut self, rate: ReportRateHz) {
        let rate = self.normalize_active_report_rate(rate);
        self.pointer.report_rate = rate;
        let Some(record) = self.current_record() else {
            debug!("no active device — report rate change kept in memory only");
            return;
        };
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        if let Some(persistent_key) = persistent_key {
            self.config
                .edit(|config| config.set_report_rate(&persistent_key, rate));
            if !self.persist_and_reload("report rate") {
                return;
            }
        } else {
            debug!(
                key = record.config_key.as_str(),
                "transient device report rate applied without persistence"
            );
        }
        if let Some(route) = route {
            self.send_ipc(crate::services::ipc::Command::SetReportRate(route, rate));
        }
    }

    #[must_use]
    pub fn report_rate(&self) -> ReportRateHz {
        self.pointer.report_rate
    }

    pub(crate) fn report_rate_status_for(&self, key: &DeviceKey) -> ReportRateStatus {
        self.pointer.reads.report_rate_status(key)
    }
}
