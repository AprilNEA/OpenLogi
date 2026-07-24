use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        onboard_profiles::{OnboardMode, OnboardProfilesFeature},
    },
};
use tracing::debug;

use crate::onboard_profiles::{OnboardProfilesInfo, ProfileEntry, ProfilesMode};
use crate::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Map the fork's `0x8100` [`OnboardMode`] onto OpenLogi's [`ProfilesMode`].
/// A future `#[non_exhaustive]` variant maps to [`ProfilesMode::Onboard`] —
/// the conservative reading that keeps the agent treating the device as
/// self-driven until told otherwise. (Reserved wire bytes never reach here —
/// the fork's `get_onboard_mode` rejects them.)
pub(super) fn onboard_mode_to_profiles(mode: OnboardMode) -> ProfilesMode {
    if matches!(mode, OnboardMode::Host) {
        ProfilesMode::Host
    } else {
        ProfilesMode::Onboard
    }
}

/// Map OpenLogi's [`ProfilesMode`] onto the fork's `0x8100` [`OnboardMode`] —
/// the inverse of [`onboard_mode_to_profiles`], used when writing the mode.
pub(super) fn profiles_to_onboard_mode(mode: ProfilesMode) -> OnboardMode {
    match mode {
        ProfilesMode::Host => OnboardMode::Host,
        ProfilesMode::Onboard => OnboardMode::Onboard,
    }
}

/// Open the `0x8100` feature on an already-open channel at HID++ `index`.
async fn open_profiles(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<(Device, Arc<OnboardProfilesFeature>), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    Ok((device, feature))
}

/// Read the full onboard-profiles state of the device addressed by `route`:
/// memory description, mode, active profile, and the profile directory.
///
/// `FeatureUnsupported` when the device has no HID++ `0x8100` — i.e. it is not
/// a gaming device with onboard profile memory.
pub async fn get_onboard_profiles(route: &DeviceRoute) -> Result<OnboardProfilesInfo, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let (_device, feature) = open_profiles(&channel, index).await?;

        let read = |e| {
            classify_hidpp_error(
                e,
                HidppOperation::ReadOnboardProfiles,
                OnboardProfilesFeature::ID,
            )
        };
        let descr = feature.get_description().await.map_err(read)?;
        let mode = feature.get_onboard_mode().await.map_err(read)?;
        let active_profile = feature.get_current_profile().await.map_err(read)?;
        let directory = feature
            .read_profile_directory(descr.profile_count)
            .await
            .map_err(read)?
            .into_iter()
            .map(|entry| ProfileEntry {
                sector: entry.sector,
                enabled: entry.enabled,
            })
            .collect();

        Ok(OnboardProfilesInfo {
            profile_count: descr.profile_count,
            profile_count_oob: descr.profile_count_oob,
            button_count: descr.button_count,
            sector_count: descr.sector_count,
            sector_size: descr.sector_size,
            memory_model_id: descr.memory_model_id,
            profile_format_id: descr.profile_format_id,
            macro_format_id: descr.macro_format_id,
            mode: onboard_mode_to_profiles(mode),
            active_profile,
            directory,
        })
    })
    .await
}

/// Write the onboard/host mode on `route` and return the read-back mode so the
/// caller can verify the firmware accepted it.
pub async fn set_profiles_mode(
    route: &DeviceRoute,
    mode: ProfilesMode,
) -> Result<ProfilesMode, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let (_device, feature) = open_profiles(&channel, index).await?;
        write_mode(&feature, index, mode).await?;
        read_mode(&feature).await
    })
    .await
}

/// Write the active profile `sector` on `route` and return the read-back
/// sector so the caller can verify the firmware accepted it.
pub async fn set_active_profile(route: &DeviceRoute, sector: u16) -> Result<u16, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let (_device, feature) = open_profiles(&channel, index).await?;
        feature.set_current_profile(sector).await.map_err(|e| {
            classify_hidpp_error(
                e,
                HidppOperation::WriteOnboardProfiles,
                OnboardProfilesFeature::ID,
            )
        })?;
        debug!(index, sector, "wrote active onboard profile");
        feature.get_current_profile().await.map_err(|e| {
            classify_hidpp_error(
                e,
                HidppOperation::ReadOnboardProfiles,
                OnboardProfilesFeature::ID,
            )
        })
    })
    .await
}

/// Apply the persisted onboard-profiles configuration to the device addressed
/// by `route`: put it in `mode`, and in onboard mode also activate `profile`
/// when given. Skips writes the device already matches, so re-applying on
/// every reconnect costs one or two reads on an already-configured device.
/// Returns whether anything was written.
///
/// The mode is volatile — devices revert to onboard mode on power cycle — so
/// the agent re-applies this whenever a device (re)appears.
pub async fn apply_profiles_config(
    route: &DeviceRoute,
    mode: ProfilesMode,
    profile: Option<u16>,
) -> Result<bool, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        apply_profiles_config_on_channel(&channel, index, mode, profile).await
    })
    .await
}

/// The config apply itself, on an already-open channel at HID++ `index`.
/// Shared by [`apply_profiles_config`] and
/// [`apply_profiles_config_on`](super::apply_profiles_config_on).
pub(super) async fn apply_profiles_config_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    mode: ProfilesMode,
    profile: Option<u16>,
) -> Result<bool, WriteError> {
    let (_device, feature) = open_profiles(channel, index).await?;

    let mut written = false;

    let current = read_mode(&feature).await?;
    if current != mode {
        write_mode(&feature, index, mode).await?;
        // Read back to confirm the firmware accepted the mode. Mirrors the DPI
        // write path: a mismatch — or a failed read-back, which happens when
        // the read races the device's mode transition (observed on a G502 X)
        // — is logged, not fatal, because the request reached the device.
        match read_mode(&feature).await {
            Ok(actual) if actual != mode => tracing::warn!(
                index,
                requested = ?mode,
                ?actual,
                "onboard mode write accepted but device reports a different mode"
            ),
            Ok(_) => {}
            Err(error) => debug!(index, %error, "onboard mode read-back skipped"),
        }
        written = true;
    }

    if mode == ProfilesMode::Onboard
        && let Some(sector) = profile
    {
        let read = |e| {
            classify_hidpp_error(
                e,
                HidppOperation::ReadOnboardProfiles,
                OnboardProfilesFeature::ID,
            )
        };
        let active = feature.get_current_profile().await.map_err(read)?;
        if active != sector {
            feature.set_current_profile(sector).await.map_err(|e| {
                classify_hidpp_error(
                    e,
                    HidppOperation::WriteOnboardProfiles,
                    OnboardProfilesFeature::ID,
                )
            })?;
            match feature.get_current_profile().await {
                Ok(actual) if actual != sector => tracing::warn!(
                    index,
                    requested = sector,
                    actual,
                    "active-profile write accepted but device reports a different sector"
                ),
                Ok(_) => {}
                Err(error) => {
                    debug!(index, %error, "active-profile read-back skipped");
                }
            }
            written = true;
        }
    }

    if written {
        debug!(index, ?mode, ?profile, "applied onboard-profiles config");
    }
    Ok(written)
}

/// Read the current mode through the OpenLogi error mapping.
async fn read_mode(feature: &Arc<OnboardProfilesFeature>) -> Result<ProfilesMode, WriteError> {
    let mode = feature.get_onboard_mode().await.map_err(|e| {
        classify_hidpp_error(
            e,
            HidppOperation::ReadOnboardProfiles,
            OnboardProfilesFeature::ID,
        )
    })?;
    Ok(onboard_mode_to_profiles(mode))
}

/// Write `mode` through the OpenLogi error mapping.
async fn write_mode(
    feature: &Arc<OnboardProfilesFeature>,
    index: u8,
    mode: ProfilesMode,
) -> Result<(), WriteError> {
    feature
        .set_onboard_mode(profiles_to_onboard_mode(mode))
        .await
        .map_err(|e| {
            classify_hidpp_error(
                e,
                HidppOperation::WriteOnboardProfiles,
                OnboardProfilesFeature::ID,
            )
        })?;
    debug!(index, ?mode, "wrote onboard mode");
    Ok(())
}
