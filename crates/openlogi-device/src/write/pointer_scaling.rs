//! HID++ `0x2205 PointerMotionScaling` reads and writes.

use std::sync::Arc;

pub use hidpp::feature::pointer_motion_scaling::PointerScaling;
use hidpp::{
    channel::HidppChannel, device::Device, feature::CreatableFeature,
    feature::pointer_motion_scaling::PointerMotionScalingFeature,
};
use tracing::debug;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// Read the pointer-motion scaling of the device `route` reaches.
pub async fn get_pointer_scaling(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<PointerScaling, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_scaling_feature(&channel, index).await?;
        read_scaling(&feature).await
    })
    .await
}

/// Write a pointer-motion scaling and return the value the device reports
/// afterwards.
///
/// The returned read-back is the applied value: the spec lets firmware clip
/// the request, though some devices store any non-zero value unchanged, so a
/// caller exposing this to users must bound `scaling` itself.
pub async fn set_pointer_scaling(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    scaling: PointerScaling,
) -> Result<PointerScaling, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_scaling_feature(&channel, index).await?;
        feature
            .set_pointer_scaling(scaling)
            .await
            .map_err(|error| {
                classify_hidpp_error(
                    error,
                    HidppOperation::WritePointerScaling,
                    PointerMotionScalingFeature::ID,
                )
            })?;
        let applied = read_scaling(&feature).await?;
        debug!(
            index,
            requested = scaling.raw(),
            applied = applied.raw(),
            "pointer scaling written"
        );
        Ok(applied)
    })
    .await
}

async fn open_scaling_feature(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<PointerMotionScalingFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<PointerMotionScalingFeature>(&mut device).await
}

async fn read_scaling(feature: &PointerMotionScalingFeature) -> Result<PointerScaling, WriteError> {
    feature.get_pointer_scaling().await.map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::ReadPointerScaling,
            PointerMotionScalingFeature::ID,
        )
    })
}
