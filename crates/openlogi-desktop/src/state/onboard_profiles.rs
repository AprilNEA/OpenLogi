//! Onboard profiles (`0x8100`): whether the device's stored profile or the
//! host controls DPI, report rate and buttons.
//!
//! Two facts, one authority each. The *setting* is `config.toml`'s per-device
//! `onboard_mode`, which the agent re-applies when the device reconnects —
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

use openlogi_core::hid::{OnboardMode, WriteError};
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

    /// The persisted onboard mode for the active device, or `None` when the
    /// user never chose one (the device keeps its own mode).
    #[must_use]
    pub fn current_onboard_mode_setting(&self) -> Option<OnboardMode> {
        self.current_record()
            .and_then(DeviceRecord::persistent_config_key)
            .and_then(|key| self.config.onboard_mode(key))
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

    /// The onboard mode the active device counts as being in — for the
    /// toggle and for whether DPI controls are offered. See [`shown_mode`].
    #[must_use]
    pub fn current_onboard_mode(&self) -> OnboardMode {
        shown_mode(
            &self.current_onboard_profiles_load(),
            self.current_onboard_mode_setting(),
        )
    }

    /// Persist `mode` for the active device and switch it there through the
    /// agent. The toggle shows the new mode at once; the device's answer
    /// arrives as [`Self::apply_onboard_mode_written`]. No-op when no device
    /// is selected or it has no onboard profiles.
    pub fn commit_onboard_mode(&mut self, mode: OnboardMode) -> StateEvents {
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
                .edit(|config| config.set_onboard_mode(&persistent_key, mode));
            if !self.persist_and_reload("onboard mode") {
                return events;
            }
        }
        let Some(route) = route else {
            return events;
        };
        self.pointer
            .reads
            .begin_onboard_profiles_write(&device_key, mode);
        self.send_ipc(crate::services::ipc::SetOnboardMode {
            route,
            mode,
            key: device_key,
        });
        events
    }

    /// The device's answer to a [`Self::commit_onboard_mode`] write: show the
    /// mode it reports, or — when it refused or could not be reached — ask it
    /// again so the optimistic value does not stand in for its own.
    pub fn apply_onboard_mode_written(
        &mut self,
        key: &DeviceKey,
        result: Result<OnboardMode, WriteError>,
    ) -> StateEvents {
        self.pointer
            .reads
            .finish_onboard_profiles_write(key, result.as_ref().copied());
        match result {
            Ok(mode) => {
                // The switch changed which DPI the sensor uses — the onboard
                // profile's, or the user's own that the agent wrote right
                // after a switch to host control. Show the user's at once,
                // then re-read either way so the cached reading, which
                // outranks the shown value on reselection, is the sensor's.
                if mode == OnboardMode::Host
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
        if !(self.current_onboard_profiles_supported()
            && self.current_onboard_mode() == OnboardMode::Onboard)
        {
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

/// The onboard mode a device counts as being in: its own reading once it has
/// landed — it is re-read on every inventory snapshot, so it follows a power
/// cycle and shows a refused switch — else the persisted setting, which the
/// agent re-applies on reconnect, else [`OnboardMode::Onboard`], the factory
/// mode.
#[must_use]
pub(crate) fn shown_mode(load: &OnboardProfilesLoad, setting: Option<OnboardMode>) -> OnboardMode {
    match load {
        OnboardProfilesLoad::Ready(mode) => **mode,
        OnboardProfilesLoad::Unknown
        | OnboardProfilesLoad::Loading
        | OnboardProfilesLoad::Failed(_)
        | OnboardProfilesLoad::Unsupported(_) => setting.unwrap_or(OnboardMode::Onboard),
    }
}
