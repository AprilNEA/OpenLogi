use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{CreatableFeature, adjustable_dpi::AdjustableDpiFeature},
    protocol::v20::{ErrorType, Hidpp20Error},
};
use tracing::debug;

use crate::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

// DpiCapabilities and DpiInfo are pure IPC wire data with no HID++ I/O, so
// they live in `openlogi_core::hid::dpi`; re-exported here unchanged so this
// module's own API surface doesn't churn.
pub use openlogi_core::hid::dpi::{DpiCapabilities, DpiInfo};

/// Read the device's current DPI on sensor 0 — companion to [`set_dpi`].
/// Used by `openlogi diag dpi` and any future Settings → Diagnostics
/// surface that wants to display the current value without writing.
pub async fn get_dpi(route: &DeviceRoute) -> Result<u16, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        get_dpi_on_channel(&channel, index).await
    })
    .await
}

async fn get_dpi_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
) -> Result<u16, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
    feature
        .get_sensor_dpi(0)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ReadDpi, AdjustableDpiFeature::ID))
}

/// Classify a HID++ error from the AdjustableDpi functions. A device that
/// announces `0x2201` but rejects a function (`Unsupported` /
/// `InvalidFunctionId`) or returns a structurally invalid DPI list
/// (`UnsupportedResponse`) will keep doing so, so these map to the permanent
/// [`WriteError::FeatureUnsupported`]; channel/timeout and other errors are
/// forwarded through [`classify_hidpp_error`] as transient so callers may retry.
fn classify_dpi_error(error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Feature(ErrorType::Unsupported | ErrorType::InvalidFunctionId)
        | Hidpp20Error::UnsupportedResponse => WriteError::FeatureUnsupported {
            feature_hex: AdjustableDpiFeature::ID,
        },
        other => classify_hidpp_error(
            other,
            HidppOperation::ReadDpiCapabilities,
            AdjustableDpiFeature::ID,
        ),
    }
}

/// Read the current DPI and the supported DPI values for sensor 0 in one
/// route/channel session.
pub async fn get_dpi_info(route: &DeviceRoute) -> Result<DpiInfo, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        get_dpi_info_on_channel(&channel, index).await
    })
    .await
}

pub(super) async fn get_dpi_info_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
) -> Result<DpiInfo, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
    let sensor_count = feature
        .get_sensor_count()
        .await
        .map_err(classify_dpi_error)?;
    if sensor_count == 0 {
        // The device claims AdjustableDpi but exposes no sensor — it cannot
        // report DPI, and that won't change on retry.
        return Err(WriteError::FeatureUnsupported {
            feature_hex: AdjustableDpiFeature::ID,
        });
    }
    let current = feature
        .get_sensor_dpi(0)
        .await
        .map_err(classify_dpi_error)?;
    let values = feature
        .get_sensor_dpi_list(0)
        .await
        .map_err(classify_dpi_error)?;
    Ok(DpiInfo {
        current,
        capabilities: DpiCapabilities::new(values)?,
    })
}

/// Set sensor 0's DPI for the device addressed by `route`.
pub async fn set_dpi(route: &DeviceRoute, dpi: u16) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_dpi_on_channel(&channel, index, dpi).await
    })
    .await
}

/// The DPI write itself, on an already-open channel at HID++ `index`. Shared by
/// [`set_dpi`] (which opens a fresh channel) and [`set_dpi_on`](super::set_dpi_on)
/// (which reuses one).
pub(super) async fn set_dpi_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
    dpi: u16,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
    feature
        .set_sensor_dpi(0, dpi)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteDpi, AdjustableDpiFeature::ID))?;
    // Read back to confirm the firmware accepted the value. A mismatch is a
    // silent failure mode that's otherwise invisible — devices in low-power
    // states or with unsupported DPI ranges can ACK the write yet keep the old
    // value. We log a warning but still return Ok because the request reached
    // the device.
    if let Ok(actual) = feature.get_sensor_dpi(0).await {
        if actual == dpi {
            debug!(index, dpi, "wrote DPI (verified)");
        } else {
            tracing::warn!(
                index,
                requested = dpi,
                actual,
                "DPI write accepted but device reports a different value — \
                 likely out of the device's supported range"
            );
        }
    } else {
        debug!(index, dpi, "wrote DPI (read-back skipped)");
    }
    Ok(())
}
