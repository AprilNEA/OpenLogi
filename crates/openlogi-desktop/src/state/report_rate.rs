//! Mouse report rate: the persisted preference and the mouse's own reading.
//!
//! The *setting* is `config.toml`'s per-device `report_rate`, which the agent
//! re-applies when the mouse reconnects. The *reading* is the swr-backed
//! device query the device-read service owns: the rate the mouse reports at
//! now and the rates it offers on its current connection. The list belongs to
//! the connection, not the mouse — a gaming mouse offers more over its
//! receiver than over Bluetooth — so the panel only ever offers what the
//! reading lists.
//!
//! A pick persists the setting and writes the mouse in one request whose
//! answer is the mouse's read-back, so the selected rate is never a guess
//! about a write still in flight: the pick shows at once, then the read-back
//! replaces it, or a refusal sends the query back to the mouse for the truth.
//! The answer names the connection it was read on and is dropped once the
//! mouse has moved to another one, whose reading is its own.

use openlogi_core::hid::{DeviceRoute, ReportRate, ReportRateInfo, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::ReportRateLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device reports a report-rate control (HID++
    /// `0x8060` or `0x8061`).
    #[must_use]
    pub fn current_report_rate_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.report_rate)
    }

    /// What is known of the active mouse's report rate.
    #[must_use]
    pub fn current_report_rate_load(&self) -> ReportRateLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.report_rate_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// Persist `rate` for the active mouse and write it through the agent.
    /// The panel shows `rate` at once; the mouse's read-back arrives as
    /// [`Self::apply_report_rate_written`]. No-op when no device is selected,
    /// it reports no report-rate control, or [`pick_target`] finds no reading
    /// that lists `rate` — the panel only offers listed rates, so that last
    /// case is a click from before the list changed.
    pub fn commit_report_rate(&mut self, rate: ReportRate) -> StateEvents {
        if !self.current_report_rate_supported() {
            return self.for_current_device(StateEvent::ReportRateChanged);
        }
        let load = self.current_report_rate_load();
        let Some(info) = pick_target(&load, rate) else {
            debug!(%rate, "the mouse's last reading does not offer this rate — ignoring the pick");
            return self.for_current_device(StateEvent::ReportRateChanged);
        };
        let supported = info.supported.clone();
        self.write_report_rate(rate, supported)
    }

    /// The write half of [`Self::commit_report_rate`], once `rate` is known
    /// to be one of the `supported` rates the active mouse last read back:
    /// persist it, show it with that list, and ask the agent to write it.
    pub(super) fn write_report_rate(
        &mut self,
        rate: ReportRate,
        supported: Vec<ReportRate>,
    ) -> StateEvents {
        let events = self.for_current_device(StateEvent::ReportRateChanged);
        let Some(record) = self.current_record() else {
            return events;
        };
        let device_key = record.device_key();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        if let Some(persistent_key) = persistent_key {
            self.config
                .edit(|config| config.set_report_rate(&persistent_key, rate));
            if !self.persist_and_reload("report rate") {
                return events;
            }
        }
        let Some(route) = route else {
            return events;
        };
        self.pointer.reads.set_report_rate_ready(
            &device_key,
            &route,
            ReportRateInfo {
                current: rate,
                supported,
            },
        );
        self.send_ipc(crate::services::ipc::SetReportRate {
            route,
            rate,
            key: device_key,
        });
        events
    }

    /// The mouse's answer to a [`Self::commit_report_rate`] write: show the
    /// read-back, or — when the mouse refused or could not be reached — ask it
    /// again so the optimistic value does not stand in for its own.
    ///
    /// `route` is the connection the write went out on. A mouse that has
    /// since moved — receiver to cable, say — keeps its key but offers that
    /// connection's rates, so only a read still subscribed on `route` takes
    /// the answer; otherwise it is dropped and the new connection's own read
    /// stands.
    pub fn apply_report_rate_written(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        result: Result<ReportRateInfo, WriteError>,
    ) -> StateEvents {
        let landed = match result {
            Ok(info) => self.pointer.reads.set_report_rate_ready(key, route, info),
            Err(error) => {
                let refreshed = self.pointer.reads.refresh_report_rate(key, route);
                if refreshed {
                    warn!(%error, key = %key, "report-rate write did not land — re-reading the mouse");
                }
                refreshed
            }
        };
        if !landed {
            debug!(key = %key, "report-rate answer for a connection no read is on — dropping it");
            return StateEvents::none();
        }
        StateEvent::ReportRateChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted report-rate read — the "click to retry"
    /// affordance on a [`ReportRateLoad::Failed`] mouse.
    pub(crate) fn retry_report_rate_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_report_rate(key);
        StateEvent::ReportRateChanged(key.clone()).into()
    }

    pub(super) fn load_current_report_rate(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.report_rate)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_report_rate(key, route, self.ipc_sender(), cx);
    }
}

/// The reading a pick of `rate` is written against: the mouse's last
/// read-back, when there is one and it lists `rate` for the current
/// connection.
pub(super) fn pick_target(load: &ReportRateLoad, rate: ReportRate) -> Option<&ReportRateInfo> {
    match load {
        ReportRateLoad::Ready(info) if info.supports(rate) => Some(info),
        ReportRateLoad::Ready(_)
        | ReportRateLoad::Unknown
        | ReportRateLoad::Loading
        | ReportRateLoad::Failed(_)
        | ReportRateLoad::Unsupported(_) => None,
    }
}
