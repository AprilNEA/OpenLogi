//! HID++ `0x1F20 AdcMeasurement` functions `1`/`2`: how many idle minutes a
//! headset waits before it switches itself off.
//!
//! The headset keeps the timeout across power cycles, so nothing here is
//! saved or written again on reconnect.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::adc_measurement::AdcMeasurementFeature,
    protocol::v20::{ErrorType, Hidpp20Error},
};
use openlogi_core::hid::PowerOffTimeout;
use tracing::debug;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// Read the automatic power-off timeout of the headset `route` reaches.
pub async fn get_power_off_timeout(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<PowerOffTimeout, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_adc_feature(&channel, index).await?;
        read(&feature).await
    })
    .await
}

/// Write `timeout` to the headset `route` reaches and return the timeout it
/// confirms.
pub async fn set_power_off_timeout(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    timeout: PowerOffTimeout,
) -> Result<PowerOffTimeout, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_adc_feature(&channel, index).await?;
        write(&feature, index, timeout).await
    })
    .await
}

/// Read the automatic power-off timeout on an already-open
/// [`SharedChannel`].
pub async fn get_power_off_timeout_on(
    shared: &SharedChannel,
) -> Result<PowerOffTimeout, WriteError> {
    let feature = open_adc_feature(shared.channel(), shared.device_index()).await?;
    read(&feature).await
}

/// Write `timeout` on an already-open [`SharedChannel`] and return the
/// timeout the headset confirms.
pub async fn set_power_off_timeout_on(
    shared: &SharedChannel,
    timeout: PowerOffTimeout,
) -> Result<PowerOffTimeout, WriteError> {
    let index = shared.device_index();
    let feature = open_adc_feature(shared.channel(), index).await?;
    write(&feature, index, timeout).await
}

/// A device can list `0x1F20` for its battery without answering function
/// `1`, and it refuses the call, so a refusal is the feature being
/// unsupported: `InvalidFunctionId`, `InvalidArgument` or `Unsupported`.
/// Every other HID++ error, `Busy` or `HwError` among them, may pass and
/// stays a retryable read error.
async fn read(feature: &AdcMeasurementFeature) -> Result<PowerOffTimeout, WriteError> {
    let raw = feature
        .get_power_off_timeout()
        .await
        .map_err(|error| match error {
            Hidpp20Error::Feature(
                ErrorType::InvalidFunctionId | ErrorType::InvalidArgument | ErrorType::Unsupported,
            ) => WriteError::FeatureUnsupported {
                feature_hex: AdcMeasurementFeature::ID,
            },
            error => classify(error, HidppOperation::ReadPowerOffTimeout),
        })?;
    parse(raw, HidppOperation::ReadPowerOffTimeout)
}

/// The confirmation is the write's own echo, as for the sidetone level.
async fn write(
    feature: &AdcMeasurementFeature,
    index: u8,
    timeout: PowerOffTimeout,
) -> Result<PowerOffTimeout, WriteError> {
    let raw = feature
        .set_power_off_timeout(timeout.minutes())
        .await
        .map_err(|error| classify(error, HidppOperation::WritePowerOffTimeout))?;
    let applied = parse(raw, HidppOperation::WritePowerOffTimeout)?;
    debug!(index, %timeout, %applied, "power-off timeout written");
    Ok(applied)
}

/// A timeout above the supported range is a reply this type cannot carry.
fn parse(raw: u8, operation: HidppOperation) -> Result<PowerOffTimeout, WriteError> {
    PowerOffTimeout::try_new(raw)
        .map_err(|_| classify(Hidpp20Error::UnsupportedResponse, operation))
}

fn classify(error: Hidpp20Error, operation: HidppOperation) -> WriteError {
    classify_hidpp_error(error, operation, AdcMeasurementFeature::ID)
}

async fn open_adc_feature(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<AdcMeasurementFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<AdcMeasurementFeature>(&mut device).await
}

#[cfg(test)]
mod tests;
