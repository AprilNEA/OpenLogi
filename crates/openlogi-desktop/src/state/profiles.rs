//! Onboard-profile reads, optimistic writes, and reconnect persistence.

use gpui::{App, Context};
use openlogi_core::config::OnboardProfiles;
use openlogi_core::hid::{OnboardProfilesInfo, ProfilesMode};
use tracing::debug;

use super::{AppState, DeviceKey, ProfilesLoad, StateEvent};

impl AppState {
    pub(super) fn load_current_profiles(&mut self, cx: &mut Context<Self>) {
        let Some((key, route)) = self
            .current_record()
            .and_then(|record| Some((record.device_key(), record.route.clone()?)))
        else {
            return;
        };
        self.reads
            .ensure_profiles(key, route, self.ipc_sender(), cx);
    }

    /// Onboard-profile status for the active device.
    #[must_use]
    pub fn current_profiles_status(&self) -> ProfilesLoad {
        self.current_record().map_or(ProfilesLoad::Unknown, |record| {
            self.reads.profiles_status(&record.device_key())
        })
    }

    /// Retry an exhausted onboard-profile read.
    pub(crate) fn retry_profiles_read(cx: &mut App, key: DeviceKey) {
        Self::update(cx, |state, cx| {
            state.retry_profiles(&key);
            cx.emit(StateEvent::ProfilesChanged(key));
        });
    }

    pub(super) fn retry_profiles(&mut self, key: &DeviceKey) {
        self.reads.retry_profiles(key);
    }

    /// Persist and apply an onboard-profile selection for the active device.
    pub(crate) fn update_onboard_profiles(
        cx: &mut App,
        mode: ProfilesMode,
        profile: Option<u16>,
    ) {
        Self::update(cx, |state, cx| {
            let key = state.current_record().map(|record| record.device_key());
            state.commit_onboard_profiles(mode, profile);
            if let Some(key) = key {
                cx.emit(StateEvent::ProfilesChanged(key));
            }
        });
    }

    fn commit_onboard_profiles(&mut self, mode: ProfilesMode, profile: Option<u16>) {
        let Some(record) = self.current_record() else {
            debug!("no active device; onboard-profile change ignored");
            return;
        };
        let key = record.device_key();
        let persistent_key = record.persistent_config_key().map(str::to_string);
        let route = record.route.clone();
        let profile = match mode {
            ProfilesMode::Host => None,
            ProfilesMode::Onboard => profile,
        };

        if let Some(persistent_key) = persistent_key {
            let config = match mode {
                ProfilesMode::Host => OnboardProfiles::Host {},
                ProfilesMode::Onboard => OnboardProfiles::Onboard { profile },
            };
            self.config
                .set_onboard_profiles(&persistent_key, Some(config));
            if !self.persist_and_reload("onboard profiles") {
                return;
            }
        }
        if let Some(route) = route {
            self.send_ipc(crate::services::ipc::Command::SetOnboardProfiles(
                route, mode, profile,
            ));
        }

        if let Some(ProfilesLoad::Ready(info)) = self.reads.profiles_load(&key) {
            let mut info: OnboardProfilesInfo = (**info).clone();
            info.mode = mode;
            match (mode, profile) {
                (ProfilesMode::Host, _) => info.active_profile = 0,
                (ProfilesMode::Onboard, Some(sector)) => info.active_profile = sector,
                (ProfilesMode::Onboard, None) => {}
            }
            self.reads.set_profiles_ready(&key, info);
            self.reads.retry_profiles(&key);
        }
    }
}
