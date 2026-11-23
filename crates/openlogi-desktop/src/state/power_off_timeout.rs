//! Headset automatic power-off: how long the headset idles before it switches
//! itself off.
//!
//! The headset stores the timeout itself and keeps it when it turns off, so
//! nothing is persisted here. The reading is the swr-backed device query the
//! device-read service owns, and the slider shows it.
//!
//! A release of the slider writes the headset. Its answer is the timeout the
//! headset confirms. A refusal reads the headset again, so the slider returns
//! to the headset's own timeout, as for sidetone.

use openlogi_core::hid::{DeviceRoute, PowerOffTimeout, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::devices::DeviceRecord;
use super::events::StateEvents;
use super::load::PowerOffTimeoutLoad;
use super::{AppState, StateEvent};

impl AppState {
    /// Whether the active device reports HID++ `0x1F20`, which carries the
    /// automatic power-off timeout.
    #[must_use]
    pub fn current_power_off_timeout_supported(&self) -> bool {
        self.current_record().is_some_and(has_power_off_timeout)
    }

    /// The route to write the active headset's automatic power-off on, or
    /// `None` when it has no `0x1F20` or is offline. A wireless headset that
    /// is switched off keeps its dongle's route, so the route alone does not
    /// say it answers.
    #[must_use]
    pub fn current_power_off_timeout_route(&self) -> Option<&DeviceRoute> {
        self.writable_power_off_timeout().map(|(_, route)| route)
    }

    fn writable_power_off_timeout(&self) -> Option<(&DeviceRecord, &DeviceRoute)> {
        let record = self
            .current_record()
            .filter(|record| has_power_off_timeout(record))?;
        Some((record, record.addressable_route()?))
    }

    /// What is known of the active headset's automatic power-off reading.
    #[must_use]
    pub fn current_power_off_timeout_load(&self) -> PowerOffTimeoutLoad {
        self.current_record()
            .and_then(|record| {
                self.pointer
                    .reads
                    .power_off_timeout_load(&record.device_key())
            })
            .cloned()
            .unwrap_or_default()
    }

    /// Write `timeout` to the active headset through the agent. The slider
    /// shows `timeout` at once; the headset's confirmation arrives as
    /// [`Self::apply_power_off_timeout_written`]. No-op when no device is
    /// selected, it has no `0x1F20`, or it is offline.
    pub fn commit_power_off_timeout(&mut self, timeout: PowerOffTimeout) -> StateEvents {
        let events = self.for_current_device(StateEvent::PowerOffTimeoutChanged);
        let Some((key, route)) = self
            .writable_power_off_timeout()
            .map(|(record, route)| (record.device_key(), route.clone()))
        else {
            return events;
        };
        self.pointer
            .reads
            .set_power_off_timeout_ready(&key, timeout);
        self.send_ipc(crate::services::ipc::SetPowerOffTimeout {
            route,
            timeout,
            key,
        });
        events
    }

    /// The headset's answer to a [`Self::commit_power_off_timeout`] write:
    /// show the confirmed timeout, or — when the headset refused or could not
    /// be reached — read it again so the optimistic value does not stand in
    /// for its own.
    pub fn apply_power_off_timeout_written(
        &mut self,
        key: &DeviceKey,
        result: Result<PowerOffTimeout, WriteError>,
    ) -> StateEvents {
        match result {
            Ok(timeout) => self.pointer.reads.set_power_off_timeout_ready(key, timeout),
            Err(error) => {
                warn!(
                    %error,
                    key = %key,
                    "power-off timeout write did not land — re-reading the headset"
                );
                self.pointer.reads.refresh_power_off_timeout(key);
            }
        }
        StateEvent::PowerOffTimeoutChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted automatic power-off read — the "click to
    /// retry" affordance on a [`PowerOffTimeoutLoad::Failed`] headset.
    pub(crate) fn retry_power_off_timeout_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_power_off_timeout(key);
        StateEvent::PowerOffTimeoutChanged(key.clone()).into()
    }

    /// `key`'s headset came back online: read it again in case another host
    /// changed its timeout.
    pub(super) fn power_off_timeout_reconnected(&mut self, key: &DeviceKey) {
        self.pointer.reads.refresh_power_off_timeout(key);
    }

    pub(super) fn load_current_power_off_timeout(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| has_power_off_timeout(record))
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        debug!(key = %key, "reading the headset's automatic power-off");
        self.pointer
            .reads
            .ensure_power_off_timeout(key, route, self.ipc_sender(), cx);
    }
}

fn has_power_off_timeout(record: &DeviceRecord) -> bool {
    record
        .capabilities
        .is_some_and(|capabilities| capabilities.power_off_timeout)
}
