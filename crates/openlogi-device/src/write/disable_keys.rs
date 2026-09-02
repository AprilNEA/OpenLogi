use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        disable_keys::{DisableKeysFeature, DisableableKeys},
    },
};

use crate::{SharedChannel, backend::HidBackend, channel::route::DeviceRoute};
use openlogi_core::hid::{DisableKeysMask, DisableKeysState};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

async fn read_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<DisableKeysState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<DisableKeysFeature>(&mut device).await?;
    read_feature(&feature).await
}

async fn read_feature(feature: &DisableKeysFeature) -> Result<DisableKeysState, WriteError> {
    let supported = feature.get_capabilities().await.map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::ReadDisableKeys,
            DisableKeysFeature::ID,
        )
    })?;
    let disabled = feature.get_disabled_keys().await.map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::ReadDisableKeys,
            DisableKeysFeature::ID,
        )
    })?;
    Ok(DisableKeysState {
        supported: DisableKeysMask::from_bits_retain(supported.bits()),
        disabled: DisableKeysMask::from_bits_retain(disabled.bits()),
    })
}

async fn set_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    desired: DisableKeysMask,
) -> Result<DisableKeysState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<DisableKeysFeature>(&mut device).await?;
    let before = read_feature(&feature).await?;
    let replacement = before.replacement_for(desired)?;
    feature
        .set_disabled_keys(DisableableKeys::from_bits_retain(replacement.bits()))
        .await
        .map_err(|error| {
            classify_hidpp_error(
                error,
                HidppOperation::WriteDisableKeys,
                DisableKeysFeature::ID,
            )
        })?;
    let after = read_feature(&feature).await?;
    let expected = replacement & before.supported;
    let actual = after.disabled & before.supported;
    if actual != expected {
        return Err(WriteError::WriteNotApplied {
            operation: HidppOperation::WriteDisableKeys,
            feature_hex: DisableKeysFeature::ID,
            expected: u64::from(expected.bits()),
            actual: u64::from(actual.bits()),
        });
    }
    Ok(after)
}

/// Read exact Disable Keys capabilities and current state for `route`.
pub async fn get_disable_keys(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<DisableKeysState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        read_on_channel(&channel, index).await
    })
    .await
}

/// Read exact Disable Keys capabilities and current state on an open channel.
pub async fn get_disable_keys_on(shared: &SharedChannel) -> Result<DisableKeysState, WriteError> {
    read_on_channel(shared.channel(), shared.device_index()).await
}

/// Replace the desired known disabled-key set for `route` after validating it.
pub async fn set_disable_keys(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    desired: DisableKeysMask,
) -> Result<DisableKeysState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_on_channel(&channel, index, desired).await
    })
    .await
}

/// Replace the desired known disabled-key set on an open channel after validation.
pub async fn set_disable_keys_on(
    shared: &SharedChannel,
    desired: DisableKeysMask,
) -> Result<DisableKeysState, WriteError> {
    set_on_channel(shared.channel(), shared.device_index(), desired).await
}
