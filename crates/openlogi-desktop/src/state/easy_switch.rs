//! Easy-Switch hosts: the computers a multi-host device keeps on its
//! channels, read from the device and renamed one at a time.
//!
//! Nothing is stored in `config.toml`. The device keeps each channel's host
//! name and OS in its own memory — the computer on the channel writes them —
//! so the swr-backed device query the device-read service owns is the only
//! record.
//!
//! A rename writes the device in one request whose answer is its read-back:
//! the new name shows at once, then the read-back replaces it, or a refusal
//! is kept for the card to explain and sends the query back to the device
//! for the truth. The answer names the connection it was read on and is
//! dropped once the device has moved to another one.

use openlogi_core::hid::{DeviceRoute, EasySwitchHosts, WriteError};
use tracing::{debug, warn};

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::EasySwitchLoad;
use super::{AppState, StateEvent};

/// Why a host rename did not land, kept until another rename is sent or one
/// lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostRenameRefusal {
    /// The zero-based slot the rename was for.
    pub(crate) slot: u8,
    /// What the agent or device answered.
    pub(crate) error: WriteError,
}

impl AppState {
    /// Whether the active device reports Easy-Switch host slots (HID++
    /// `0x1815`).
    #[must_use]
    pub fn current_easy_switch_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.easy_switch)
    }

    /// Whether a rename can reach the active device now: it has a connection
    /// and is awake on it. A receiver device that is asleep, out of range or
    /// switched to another computer keeps its route but answers nothing.
    #[must_use]
    pub fn current_easy_switch_reachable(&self) -> bool {
        self.current_record()
            .is_some_and(|record| record.online && record.route.is_some())
    }

    /// What is known of the active device's Easy-Switch hosts.
    #[must_use]
    pub fn current_easy_switch_load(&self) -> EasySwitchLoad {
        self.current_record()
            .and_then(|record| self.pointer.reads.easy_switch_load(&record.device_key()))
            .cloned()
            .unwrap_or_default()
    }

    /// Why the active device's last host rename did not land, if it did not
    /// and nothing was renamed since.
    #[must_use]
    pub(crate) fn current_host_rename_refusal(&self) -> Option<HostRenameRefusal> {
        self.current_record()
            .and_then(|record| self.pointer.reads.host_rename_refusal(&record.device_key()))
            .cloned()
    }

    /// Name the computer on Easy-Switch `slot` (zero-based) of the device
    /// `key` through the agent. The card shows the name at once; the
    /// device's read-back arrives as [`Self::apply_easy_switch_host_renamed`].
    ///
    /// No-op unless `key` is the active device, it reports Easy-Switch hosts
    /// and is reachable (see [`Self::current_easy_switch_reachable`]), its
    /// last reading accepts `name` for `slot` (see
    /// [`EasySwitchHosts::name_for`]) and `name` differs from the name the
    /// slot shows, and that reading is on the device's current connection.
    /// The rename dialog checks the name first, so those are renames from
    /// before the reading moved on.
    pub fn rename_easy_switch_host(
        &mut self,
        key: &DeviceKey,
        slot: u8,
        name: &str,
    ) -> StateEvents {
        let events = self.for_current_device(StateEvent::EasySwitchChanged);
        if !self.current_easy_switch_supported() {
            return events;
        }
        if !self.is_current_device(key) {
            debug!(key = %key, "host rename for a device that is no longer selected — ignoring it");
            return events;
        }
        if !self.current_easy_switch_reachable() {
            debug!(key = %key, "host rename for a device that is offline — ignoring it");
            return events;
        }
        let Some(route) = self
            .current_record()
            .and_then(|record| record.route.clone())
        else {
            return events;
        };
        let EasySwitchLoad::Ready(hosts) = self.current_easy_switch_load() else {
            return events;
        };
        let Some(name) = rename_target(&hosts, slot, name) else {
            debug!(
                slot,
                "the device's last reading does not take this name — ignoring it"
            );
            return events;
        };
        let Some(write) = self
            .pointer
            .reads
            .begin_host_rename(key, &route, slot, name)
        else {
            debug!(
                slot,
                "the Easy-Switch reading is not on the device's connection yet — ignoring the rename"
            );
            return events;
        };
        self.send_ipc(crate::services::ipc::RenameEasySwitchHost {
            route,
            slot,
            name: name.to_owned(),
            key: key.clone(),
            write,
        });
        events
    }

    /// The device's answer to the [`Self::rename_easy_switch_host`] rename
    /// numbered `write`: show the read-back, with any rename sent since still
    /// shown, or keep the refusal for the card and ask the device again so
    /// the shown name does not stand in for its own.
    ///
    /// `route` is the connection the rename went out on. Only a read still
    /// subscribed on `route` takes the answer; once the device has moved, the
    /// new connection's own read stands. An answer arriving after a newer
    /// rename's answer is dropped too.
    pub fn apply_easy_switch_host_renamed(
        &mut self,
        key: &DeviceKey,
        route: &DeviceRoute,
        write: u64,
        result: Result<EasySwitchHosts, WriteError>,
    ) -> StateEvents {
        if let Err(error) = &result {
            warn!(%error, key = %key, "host rename did not land — re-reading the device");
        }
        if !self
            .pointer
            .reads
            .finish_host_rename(key, route, write, result)
        {
            debug!(key = %key, write, "host rename answer superseded, or for a connection no read is on — dropping it");
            return StateEvents::none();
        }
        StateEvent::EasySwitchChanged(key.clone()).into()
    }

    /// Re-run `key`'s exhausted Easy-Switch read — the "click to retry"
    /// affordance on a [`EasySwitchLoad::Failed`] device.
    pub(crate) fn retry_easy_switch_read(&mut self, key: &DeviceKey) -> StateEvents {
        self.pointer.reads.retry_easy_switch(key);
        StateEvent::EasySwitchChanged(key.clone()).into()
    }

    pub(super) fn load_current_easy_switch(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .filter(|record| {
                record
                    .capabilities
                    .is_some_and(|capabilities| capabilities.easy_switch)
            })
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.pointer
            .reads
            .ensure_easy_switch(key, route, self.ipc_sender(), cx);
    }
}

/// The name to write when `name` is asked for `slot`: what
/// [`EasySwitchHosts::name_for`] accepts, unless the slot already shows it.
pub(crate) fn rename_target<'a>(
    hosts: &EasySwitchHosts,
    slot: u8,
    name: &'a str,
) -> Option<&'a str> {
    let name = hosts.name_for(slot, name).ok()?;
    let shown = hosts.slot(slot).and_then(|entry| entry.name.as_deref());
    (shown != Some(name)).then_some(name)
}
