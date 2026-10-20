//! Onboard profiles (`0x8100`): whether the device's stored profile or the
//! host controls DPI, report rate and buttons.
//!
//! Two facts, one authority each. The *setting* is `config.toml`'s per-device
//! `onboard_profiles`, which the agent re-applies when the device reconnects —
//! the PRO X3 SUPERSTRIKE falls back to onboard mode on every power cycle. The
//! *reading* is the swr-backed device query the device-read service owns,
//! re-read on every inventory snapshot so a power cycle cannot leave it stale.
//! What the toggle shows — and whether DPI controls are offered — follows the
//! reading; the setting only stands in until one lands.
//!
//! A toggle flip persists the setting and writes the device in one request
//! whose answer is the mode it reports afterwards: the toggle shows the
//! asked-for value at once, then the answer replaces it, or a refusal sends
//! the query back to the device, which then shows the mode it kept.

use openlogi_core::hid::WriteError;
use tracing::warn;

use super::device_key::DeviceKey;
use super::events::StateEvents;
use super::load::OnboardProfilesLoad;
use super::{AppState, StateEvent};
use crate::state::devices::DeviceRecord;

impl AppState {
    /// Whether the active device stores onboard profiles (HID++ `0x8100`).
    #[must_use]
    pub fn current_onboard_profiles_supported(&self) -> bool {
        self.current_record()
            .and_then(|record| record.capabilities)
            .is_some_and(|capabilities| capabilities.onboard_profiles)
    }

    /// The persisted onboard-profiles choice for the active device, or `None`
    /// when the user never set one (the device keeps its own mode).
    #[must_use]
    pub fn current_onboard_profiles_setting(&self) -> Option<bool> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.onboard_profiles(key))
    }

    /// What is known of the active device's onboard mode.
    #[must_use]
    pub fn current_onboard_profiles_load(&self) -> OnboardProfilesLoad {
        self.current_record()
            .and_then(|record| {
                self.pointer
                    .reads
                    .onboard_profiles_load(&record.device_key())
            })
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the onboard profiles of the active device count as active —
    /// for the toggle and for whether DPI controls are offered. See
    /// [`onboard_profiles_shown`].
    #[must_use]
    pub fn current_onboard_profiles_shown(&self) -> bool {
        onboard_profiles_shown(
            &self.current_onboard_profiles_load(),
            self.current_onboard_profiles_setting(),
        )
    }

    /// Persist the onboard-profiles choice for the active device and switch it
    /// between its onboard profiles (`true`) and host control (`false`)
    /// through the agent. The toggle shows the new
    /// value at once; the device's answer arrives as
    /// [`Self::apply_onboard_profiles_written`]. No-op when no device is
    /// selected or it has no onboard profiles.
    pub fn commit_onboard_profiles(&mut self, onboard_profiles: bool) -> StateEvents {
        let events = self.for_current_device(StateEvent::OnboardProfilesChanged);
        if !self.current_onboard_profiles_supported() {
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
                .edit(|config| config.set_onboard_profiles(&persistent_key, onboard_profiles));
            if !self.persist_and_reload("onboard-profiles") {
                return events;
            }
        }
        let Some(route) = route else {
            return events;
        };
        self.pointer
            .reads
            .begin_onboard_profiles_write(&device_key, onboard_profiles);
        self.send_ipc(crate::services::ipc::SetOnboardProfiles {
            route,
            onboard_profiles,
            key: device_key,
        });
        events
    }

    /// The device's answer to a [`Self::commit_onboard_profiles`] write: show
    /// the mode it reports, or — when it refused or could not be reached — ask
    /// it again so the optimistic value does not stand in for its own.
    pub fn apply_onboard_profiles_written(
        &mut self,
        key: &DeviceKey,
        result: Result<bool, WriteError>,
    ) -> StateEvents {
        self.pointer
            .reads
            .finish_onboard_profiles_write(key, result.as_ref().copied());
        match result {
            Ok(onboard_profiles) => {
                // The switch changed which DPI the sensor uses — the onboard
                // profile's, or the user's own that the agent wrote right
                // after a switch to host control. Show the user's at once,
                // then re-read either way so the cached reading, which
                // outranks the shown value on reselection, is the sensor's.
                if !onboard_profiles
                    && self.is_current_device(key)
                    && let Some(dpi) = self.current_configured_dpi()
                {
                    self.pointer.dpi = dpi;
                }
                self.pointer.reads.reread_dpi(key);
            }
            Err(error) => {
                warn!(%error, key = %key, "onboard-mode write did not land — re-reading the device");
            }
        }
        StateEvent::OnboardProfilesChanged(key.clone()).into()
    }

    /// The DPI persisted for the active device on its current link.
    fn current_configured_dpi(&self) -> Option<openlogi_core::hid::Dpi> {
        let record = self.current_record()?;
        self.config
            .devices
            .get(record.persistent_config_key()?)?
            .effective_dpi(&record.route_key)
    }

    /// Re-read the active device's onboard mode on every completed inventory
    /// snapshot: a power cycle drops it back to onboard mode, and the agent's
    /// re-apply may or may not have landed by the time the GUI looks.
    pub(super) fn refresh_current_onboard_profiles(&mut self) {
        if !self.current_onboard_profiles_supported() {
            return;
        }
        if let Some(key) = self.current_record().map(DeviceRecord::device_key) {
            self.pointer.reads.refresh_onboard_profiles(&key);
        }
    }

    /// While the active device's onboard profile owns its DPI, re-read it on
    /// every completed inventory snapshot: the profile can change it behind
    /// the host's back (a power cycle, the device's own DPI button).
    pub(super) fn refresh_onboard_dpi(&mut self) {
        if !(self.current_onboard_profiles_supported() && self.current_onboard_profiles_shown()) {
            return;
        }
        if let Some(key) = self.current_record().map(DeviceRecord::device_key) {
            self.pointer.reads.reread_dpi(&key);
        }
    }

    pub(super) fn load_current_onboard_profiles(&mut self, cx: &mut gpui::Context<Self>) {
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
            .ensure_onboard_profiles(key, route, self.ipc_sender(), cx);
    }
}

/// Whether onboard profiles count as active: the device's own reading once it
/// has landed — it is re-read on every inventory snapshot, so it follows a
/// power cycle and shows a refused switch — else the persisted setting, which
/// the agent re-applies on reconnect, else on, the factory mode.
#[must_use]
pub(crate) fn onboard_profiles_shown(load: &OnboardProfilesLoad, setting: Option<bool>) -> bool {
    match load {
        OnboardProfilesLoad::Ready(onboard_profiles) => **onboard_profiles,
        OnboardProfilesLoad::Unknown
        | OnboardProfilesLoad::Loading
        | OnboardProfilesLoad::Failed(_)
        | OnboardProfilesLoad::Unsupported(_) => setting.unwrap_or(true),
    }
}
