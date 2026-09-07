//! HID++ `HeadsetAudioSidetone` (feature `0x0604`) write operations.

use std::sync::Arc;

use hidpp::channel::HidppChannel;
use hidpp::device::Device;
use hidpp::feature::CreatableFeature;
use hidpp::feature::headset_audio_sidetone::HeadsetAudioSidetoneFeature;

use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, with_route};

/// Reads the headset sidetone level (percentage 0..=100) on the device `route` reaches.
pub async fn get_sidetone_level(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<u8, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_sidetone_level_on_channel(&channel, index).await
    })
    .await
}

/// Sets the headset sidetone level (percentage 0..=100) on the device `route` reaches.
pub async fn set_sidetone_level(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    level: u8,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_sidetone_level_on_channel(&channel, index, level).await
    })
    .await
}

/// Reads the headset sidetone level on an already-open channel.
pub async fn get_sidetone_level_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<u8, WriteError> {
    if channel.product_id == 0x0af7 {
        let feature = HeadsetAudioSidetoneFeature::new(Arc::clone(channel), index, 7);
        return feature
            .get_sidetone_level(0)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")));
    }

    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let info = device
        .root()
        .get_feature(HeadsetAudioSidetoneFeature::ID)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, HeadsetAudioSidetoneFeature::ID))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex: HeadsetAudioSidetoneFeature::ID })?;

    let feature = device.add_feature::<HeadsetAudioSidetoneFeature>(info.index);
    feature
        .get_sidetone_level(info.version)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))
}

/// Sets the headset sidetone level on an already-open channel.
pub async fn set_sidetone_level_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    level: u8,
) -> Result<(), WriteError> {
    if channel.product_id == 0x0af7 {
        let feature = HeadsetAudioSidetoneFeature::new(Arc::clone(channel), index, 7);
        return feature
            .set_sidetone_level(0, level)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")));
    }

    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let info = device
        .root()
        .get_feature(HeadsetAudioSidetoneFeature::ID)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, HeadsetAudioSidetoneFeature::ID))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex: HeadsetAudioSidetoneFeature::ID })?;

    let feature = device.add_feature::<HeadsetAudioSidetoneFeature>(info.index);
    feature
        .set_sidetone_level(info.version, level)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))
}
