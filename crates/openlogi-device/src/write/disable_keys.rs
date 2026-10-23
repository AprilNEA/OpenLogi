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

use super::{
    HidppOperation, WriteError, classify_hidpp_error, open_feature, open_feature_write_through,
    with_route,
};

enum Operation<'a> {
    Bounded,
    Owned(&'a (dyn Fn() -> Result<(), WriteError> + Sync)),
}

impl Operation<'_> {
    fn checkpoint(&self) -> Result<(), WriteError> {
        match self {
            Self::Bounded => Ok(()),
            Self::Owned(check) => check(),
        }
    }
}

async fn read_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<DisableKeysState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<DisableKeysFeature>(&mut device).await?;
    read_feature(&feature, &Operation::Bounded).await
}

async fn read_feature(
    feature: &DisableKeysFeature,
    operation: &Operation<'_>,
) -> Result<DisableKeysState, WriteError> {
    operation.checkpoint()?;
    let supported = match operation {
        Operation::Bounded => feature.get_capabilities().await,
        Operation::Owned(_) => feature.get_capabilities_write_through().await,
    }
    .map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::ReadDisableKeys,
            DisableKeysFeature::ID,
        )
    })?;
    operation.checkpoint()?;
    let disabled = match operation {
        Operation::Bounded => feature.get_disabled_keys().await,
        Operation::Owned(_) => feature.get_disabled_keys_write_through().await,
    }
    .map_err(|error| {
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
    operation: Operation<'_>,
) -> Result<DisableKeysState, WriteError> {
    operation.checkpoint()?;
    let mut device = match operation {
        Operation::Bounded => Device::new(Arc::clone(channel), index).await,
        Operation::Owned(_) => Device::new_write_through(Arc::clone(channel), index).await,
    }
    .map_err(|_| WriteError::DeviceUnreachable { index })?;
    operation.checkpoint()?;
    let feature = match operation {
        Operation::Bounded => open_feature::<DisableKeysFeature>(&mut device).await,
        Operation::Owned(_) => open_feature_write_through::<DisableKeysFeature>(&mut device).await,
    }?;
    let before = read_feature(&feature, &operation).await?;
    let replacement = before.replacement_for(desired)?;
    operation.checkpoint()?;
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
    let after = read_feature(&feature, &operation).await?;
    operation.checkpoint()?;
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
        set_on_channel(&channel, index, desired, Operation::Bounded).await
    })
    .await
}

/// Replace the desired known disabled-key set on an open channel after validation.
pub async fn set_disable_keys_on(
    shared: &SharedChannel,
    desired: DisableKeysMask,
) -> Result<DisableKeysState, WriteError> {
    set_on_channel(
        shared.channel(),
        shared.device_index(),
        desired,
        Operation::Bounded,
    )
    .await
}

/// Replace keys in an independently owned transaction. Every submitted report,
/// including discovery, queries and readback, drains its native write before
/// returning. `check` runs between reports to reject cancelled or retired work.
///
/// Drive this future to completion even after the requester stops waiting.
/// A stuck native driver deliberately retains the owner's guards; only the
/// response wait is bounded, never a native operation without cancellation.
pub async fn set_disable_keys_write_through_on(
    shared: &SharedChannel,
    desired: DisableKeysMask,
    check: impl Fn() -> Result<(), WriteError> + Sync,
) -> Result<DisableKeysState, WriteError> {
    set_on_channel(
        shared.channel(),
        shared.device_index(),
        desired,
        Operation::Owned(&check),
    )
    .await
}
