use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        haptic_feedback::{
            HapticConfiguration, HapticFeedbackFeature, HapticIntensity, HapticWaveform,
        },
    },
};

use crate::route::DeviceRoute;

use super::{
    HidppOperation, SharedChannel, WriteError, classify_hidpp_error, open_feature, with_route,
};

async fn feature_on_channel(
    channel: &Arc<HidppChannel>,
    device_index: u8,
) -> Result<Arc<HapticFeedbackFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: device_index,
        })?;
    open_feature::<HapticFeedbackFeature>(&mut device).await
}

/// Read the current device-wide haptic configuration on an open capture channel.
pub async fn get_haptic_configuration_on(
    shared: &SharedChannel,
) -> Result<HapticConfiguration, WriteError> {
    let feature = feature_on_channel(shared.channel(), shared.device_index()).await?;
    feature.get_configuration().await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::ReadHaptic, HapticFeedbackFeature::ID)
    })
}

/// Read the current device-wide haptic configuration by route.
pub async fn get_haptic_configuration(
    route: &DeviceRoute,
) -> Result<HapticConfiguration, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let feature = feature_on_channel(&channel, index).await?;
        feature.get_configuration().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::ReadHaptic, HapticFeedbackFeature::ID)
        })
    })
    .await
}

/// Set the device-wide haptic configuration on an open capture channel.
pub async fn set_haptic_configuration_on(
    shared: &SharedChannel,
    enabled: bool,
    intensity: HapticIntensity,
) -> Result<(), WriteError> {
    let feature = feature_on_channel(shared.channel(), shared.device_index()).await?;
    feature
        .set_configuration(enabled, intensity)
        .await
        .map_err(|error| {
            classify_hidpp_error(
                error,
                HidppOperation::WriteHaptic,
                HapticFeedbackFeature::ID,
            )
        })
}

/// Set the device-wide haptic configuration by route.
pub async fn set_haptic_configuration(
    route: &DeviceRoute,
    enabled: bool,
    intensity: HapticIntensity,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let feature = feature_on_channel(&channel, index).await?;
        feature
            .set_configuration(enabled, intensity)
            .await
            .map_err(|error| {
                classify_hidpp_error(
                    error,
                    HidppOperation::WriteHaptic,
                    HapticFeedbackFeature::ID,
                )
            })
    })
    .await
}

/// Play a waveform immediately on an open capture channel.
pub async fn play_haptic_on(
    shared: &SharedChannel,
    waveform: HapticWaveform,
) -> Result<(), WriteError> {
    let feature = feature_on_channel(shared.channel(), shared.device_index()).await?;
    feature.play(waveform).await.map_err(|error| {
        classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
    })
}

/// Play a waveform immediately by route.
pub async fn play_haptic(route: &DeviceRoute, waveform: HapticWaveform) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let feature = feature_on_channel(&channel, index).await?;
        feature.play(waveform).await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::PlayHaptic, HapticFeedbackFeature::ID)
        })
    })
    .await
}
