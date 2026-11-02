//! HID++ `0x2205 PointerMotionScaling` reads and writes.
//!
//! [`get_pointer_scaling`] and [`set_pointer_scaling`] take any raw factor,
//! for diagnostics. The agent uses [`get_pointer_speed_on`] and
//! [`set_pointer_speed_on`], which carry a bounded [`PointerSpeed`] only.

use std::sync::Arc;

pub use hidpp::feature::pointer_motion_scaling::PointerScaling;
use hidpp::{
    channel::HidppChannel, device::Device, feature::CreatableFeature,
    feature::pointer_motion_scaling::PointerMotionScalingFeature,
};
use openlogi_core::hid::PointerSpeed;
use tracing::debug;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::SharedChannel;
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

/// Read the pointer speed of the device on an already-open
/// [`SharedChannel`].
///
/// The firmware stores any non-zero factor (#1372), so a reading outside
/// [`PointerSpeed`]'s range is clamped into it.
pub async fn get_pointer_speed_on(shared: &SharedChannel) -> Result<PointerSpeed, WriteError> {
    let index = shared.device_index();
    let feature = open_scaling_feature(shared.channel(), index).await?;
    let scaling = read_scaling(&feature).await?;
    Ok(speed_of(index, scaling))
}

/// Write `speed` on an already-open [`SharedChannel`] and return the speed
/// the device reports afterwards.
///
/// The write was acknowledged when the read-back fails, so the requested
/// speed is returned then, as the DPI and report-rate writes do.
pub async fn set_pointer_speed_on(
    shared: &SharedChannel,
    speed: PointerSpeed,
) -> Result<PointerSpeed, WriteError> {
    let index = shared.device_index();
    let Some(scaling) = PointerScaling::from_raw(speed.into_inner()) else {
        unreachable!("a pointer speed is never zero");
    };
    let feature = open_scaling_feature(shared.channel(), index).await?;
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
    match read_scaling(&feature).await {
        Ok(applied) => {
            debug!(index, %speed, applied = applied.raw(), "pointer speed written");
            Ok(speed_of(index, applied))
        }
        Err(error) => {
            debug!(index, %speed, %error, "pointer speed written (read-back skipped)");
            Ok(speed)
        }
    }
}

/// The [`PointerSpeed`] nearest to what the device reported.
fn speed_of(index: u8, scaling: PointerScaling) -> PointerSpeed {
    let speed = PointerSpeed::clamped(scaling.raw());
    if speed.into_inner() != scaling.raw() {
        debug!(
            index,
            reported = scaling.raw(),
            %speed,
            "pointer scaling outside the supported range"
        );
    }
    speed
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

#[cfg(test)]
mod tests;
