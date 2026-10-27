//! G-series onboard memory reads; the capture session owns the writes.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::onboard_profiles::{self, OnboardProfilesFeature},
};
use openlogi_core::hid::{OnboardMode, OnboardProfile, OnboardState, ProfileIndex};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Read the onboard memory of the device on `route`.
pub async fn get_onboard(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<OnboardState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_onboard_on_channel(&channel, index).await
    })
    .await
}

/// Read the onboard memory on an open [`SharedChannel`].
pub async fn get_onboard_on(shared: &SharedChannel) -> Result<OnboardState, WriteError> {
    get_onboard_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_onboard_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<OnboardState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let onboard = open_feature::<OnboardProfilesFeature>(&mut device).await?;
    let err = |e| classify_hidpp_error(e, HidppOperation::ReadOnboard, 0x8100);
    let info = onboard.get_info().await.map_err(err)?;
    let mode = match onboard.get_mode().await.map_err(err)? {
        onboard_profiles::OnboardMode::Onboard => OnboardMode::Onboard,
        onboard_profiles::OnboardMode::Host => OnboardMode::Host,
    };
    let active_profile = onboard
        .get_current_profile()
        .await
        .map_err(err)?
        .map(ProfileIndex::from);
    let mut profiles = Vec::new();
    for entry in onboard.read_directory(&info).await.map_err(err)? {
        let name = onboard
            .read_profile_name(&info, entry.sector)
            .await
            .unwrap_or_else(|error| {
                debug!(profile = %entry.index, ?error, "profile name read failed");
                None
            });
        profiles.push(OnboardProfile {
            index: entry.index.into(),
            name,
            enabled: entry.enabled,
        });
    }
    Ok(OnboardState {
        mode,
        active_profile,
        profiles,
    })
}
