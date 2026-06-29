//! HID++ `0x2121 HiResWheel` writes.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::hires_wheel::{HiResWheelFeature, WheelEventTarget},
};
use tracing::debug;

use crate::route::DeviceRoute;
use crate::write::{SharedChannel, WriteError, open_feature, with_route};

/// Write the device's native vertical-scroll inversion flag.
///
/// HID++ `0x2121` applies this flag while wheel movement is reported through
/// native HID, so the OS still receives ordinary scroll events but the direction
/// has already been transformed by the mouse firmware. That preserves true
/// per-device semantics even when several mice share one receiver.
///
/// Returns [`WriteError::FeatureUnsupported`] when the device lacks `0x2121` or
/// reports that native inversion is not supported.
pub async fn set_scroll_inversion(route: &DeviceRoute, inverted: bool) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_scroll_inversion_on_channel(&channel, index, inverted).await
    })
    .await
}

async fn set_scroll_inversion_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    inverted: bool,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<HiResWheelFeature>(&mut device).await?;
    let capabilities = feature
        .get_wheel_capabilities()
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    if !capabilities.has_invert {
        return Err(WriteError::FeatureUnsupported {
            feature_hex: HiResWheelFeature::ID,
        });
    }
    let mode = feature
        .get_wheel_mode()
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    // Idempotent: the desired state is "native HID reporting with this invert
    // flag". When the wheel already holds it, skip the write — config reloads
    // fire on every DPI / SmartShift / binding change, and re-writing the wheel
    // mode each time is needless HID++ traffic that can race other writes.
    if mode.inverted == inverted && mode.target == WheelEventTarget::Native {
        debug!(
            index,
            inverted, "native scroll inversion already set; skipping"
        );
        return Ok(());
    }
    let written = feature
        .set_wheel_mode(WheelEventTarget::Native, mode.resolution, inverted)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    debug!(
        index,
        inverted,
        resolution = ?written.resolution,
        target = ?written.target,
        "wrote native scroll inversion"
    );
    Ok(())
}

/// Write native scroll inversion on an already-open [`SharedChannel`].
pub async fn set_scroll_inversion_on(
    shared: &SharedChannel,
    inverted: bool,
) -> Result<(), WriteError> {
    set_scroll_inversion_on_channel(shared.channel(), shared.device_index(), inverted).await
}
