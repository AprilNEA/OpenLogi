//! Onboard-profile lazy reads, optimistic writes, and reconnect persistence.

use openlogi_core::config::OnboardProfiles;
use openlogi_core::hid::{DeviceRoute, OnboardProfilesInfo, ProfilesMode, WriteError};
use tracing::debug;

use super::AppState;
use super::device_key::DeviceKey;
use super::load::ProfilesLoad;

impl AppState {
    /// Onboard-profile status for the active device.
    #[must_use]
    pub fn current_profiles_status(&self) -> ProfilesLoad {
        self.current_record()
            .map_or(ProfilesLoad::Unknown, |record| {
                self.reads.profiles.status(&record.device_key())
            })
    }

    /// Whether the active device has never had its onboard-profile state read.
    #[must_use]
    pub fn current_profiles_unqueried(&self) -> bool {
        self.current_record()
            .is_some_and(|record| self.reads.profiles.unqueried(&record.device_key()))
    }

    /// Drop `key`'s failed state so the panel's next render retries the read.
    pub fn retry_profiles(&mut self, key: &DeviceKey) {
        self.reads.profiles.retry(key);
    }

    /// Store a read only while its physical device and route still match.
    pub fn store_profiles_info(
        &mut self,
        key: DeviceKey,
        route: &DeviceRoute,
        result: Result<OnboardProfilesInfo, WriteError>,
    ) {
        let matches_route = self
            .device_list
            .iter()
            .any(|record| record.device_key() == key && record.route.as_ref() == Some(route));
        let still_present = self
            .device_list
            .iter()
            .any(|record| record.device_key() == key);
        self.reads.profiles.store(
            key,
            result,
            profiles_error_is_permanent,
            matches_route,
            still_present,
            "onboard profiles",
        );
    }

    /// Persist and apply an onboard-profile mode for the active device, then
    /// optimistically update its cached state until a confirming read lands.
    pub fn commit_onboard_profiles(&mut self, mode: ProfilesMode, profile: Option<u16>) {
        let Some(record) = self.current_record() else {
            debug!("no active device — onboard-profile change ignored");
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
        if let Some(route) = route.clone() {
            self.send_ipc(crate::services::ipc::Command::SetOnboardProfiles(
                route, mode, profile,
            ));
        }

        if let Some(ProfilesLoad::Ready(info)) = self.reads.profiles.get(&key) {
            let mut info = info.clone();
            info.mode = mode;
            match (mode, profile) {
                (ProfilesMode::Host, _) => info.active_profile = 0,
                (ProfilesMode::Onboard, Some(sector)) => info.active_profile = sector,
                (ProfilesMode::Onboard, None) => {}
            }
            self.reads.profiles.set_ready(key.clone(), info);
        }
        if route.is_some() {
            self.device_ui
                .entry(key)
                .or_default()
                .profiles_pending_confirm = true;
        }
    }

    /// Take the active device's one-shot post-write confirmation target.
    pub fn take_active_profiles_confirm(&mut self) -> Option<(DeviceKey, DeviceRoute)> {
        let record = self.current_record()?;
        let key = record.device_key();
        let route = record.route.clone()?;
        let pending = &mut self.device_ui.get_mut(&key)?.profiles_pending_confirm;
        if !std::mem::take(pending) {
            return None;
        }
        Some((key, route))
    }
}

fn profiles_error_is_permanent(error: &WriteError) -> bool {
    matches!(error, WriteError::FeatureUnsupported { .. })
}

#[cfg(test)]
mod tests {
    use openlogi_core::hid::{HidppOperation, WriteError};

    use super::profiles_error_is_permanent;

    #[test]
    fn only_missing_feature_is_a_permanent_profiles_read_error() {
        assert!(profiles_error_is_permanent(
            &WriteError::FeatureUnsupported {
                feature_hex: 0x8100
            }
        ));
        assert!(!profiles_error_is_permanent(&WriteError::RequestTimedOut {
            operation: HidppOperation::ReadOnboardProfiles
        }));
    }
}
