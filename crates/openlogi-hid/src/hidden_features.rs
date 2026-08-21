//! Route-level diagnostics for `0x1e00 EnableHiddenFeatures`, raw HID++ calls,
//! and the `0x19c0 ForceSensingButton` probe surface on the MX Master 4.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use hidpp::device::Device;
use hidpp::feature::enable_hidden_features::EnableHiddenFeaturesFeature;
use hidpp::feature::force_sensing_button::ForceSensingButtonFeature;
use thiserror::Error;

use crate::channel::route::{DeviceRoute, open_route_channel};
use crate::write::open_feature;

/// Hard wall-clock budget for one whole diagnostic (open + calls). A cold
/// BTLE link can swallow a request without ever answering, and the underlying
/// channel read has no timeout of its own — without this bound a single call
/// can hang a diagnostic process forever (observed on real hardware).
const DIAG_BUDGET: Duration = Duration::from_secs(8);

async fn bounded<T>(
    fut: impl Future<Output = Result<T, HiddenDiagError>>,
) -> Result<T, HiddenDiagError> {
    tokio::time::timeout(DIAG_BUDGET, fut)
        .await
        .map_err(|_| HiddenDiagError::Hidpp("call timed out (link cold?)".into()))?
}

/// Failure modes for the hidden-features / force-button diagnostics.
#[derive(Debug, Error)]
pub enum HiddenDiagError {
    /// No HID node matched the route.
    #[error("no connected device matched the route")]
    DeviceNotFound,
    /// Transport-level failure opening the route.
    #[error("HID transport error: {0}")]
    Hid(String),
    /// The node opened but the HID++ device index did not answer.
    #[error("device at index {index:#04x} did not respond to HID++")]
    DeviceUnreachable {
        /// HID++ device index that failed to answer.
        index: u8,
    },
    /// A feature lookup or call failed.
    #[error("HID++ error: {0}")]
    Hidpp(String),
}

async fn open_device(route: &DeviceRoute) -> Result<Device, HiddenDiagError> {
    let chan = open_route_channel(route)
        .await
        .map_err(|e| HiddenDiagError::Hid(format!("{e:?}")))?
        .ok_or(HiddenDiagError::DeviceNotFound)?;
    let index = route.device_index();
    Device::new(Arc::clone(&chan), index)
        .await
        .map_err(|_| HiddenDiagError::DeviceUnreachable { index })
}

/// Reads the current `0x1e00` enabled state.
pub async fn hidden_features_enabled(route: &DeviceRoute) -> Result<bool, HiddenDiagError> {
    bounded(async {
        let mut device = open_device(route).await?;
        let feature = open_feature::<EnableHiddenFeaturesFeature>(&mut device)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(e.to_string()))?;
        feature
            .get_enabled()
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Writes the `0x1e00` enabled state and returns the read-back value.
pub async fn set_hidden_features_enabled(
    route: &DeviceRoute,
    enabled: bool,
) -> Result<bool, HiddenDiagError> {
    bounded(async {
        let mut device = open_device(route).await?;
        let feature = open_feature::<EnableHiddenFeaturesFeature>(&mut device)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(e.to_string()))?;
        feature
            .set_enabled(enabled)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))?;
        feature
            .get_enabled()
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Sends one raw short-form call to ANY HID++ 2.0 feature by ID. Returns
/// `Ok(None)` when the device does not expose the feature.
/// Reverse-engineering aid; interpretation is the caller's.
pub async fn raw_feature_call(
    route: &DeviceRoute,
    feature_id: u16,
    function: u8,
    args: [u8; 3],
) -> Result<Option<[u8; 16]>, HiddenDiagError> {
    bounded(async {
        let device = open_device(route).await?;
        device
            .raw_feature_call(feature_id, function, args)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Sends one raw `0x19c0 ForceSensingButton` call and returns the 16-byte
/// response payload. Reverse-engineering aid; interpretation is the caller's.
pub async fn force_button_raw_call(
    route: &DeviceRoute,
    function: u8,
    args: [u8; 3],
) -> Result<[u8; 16], HiddenDiagError> {
    bounded(async {
        let mut device = open_device(route).await?;
        let feature = open_feature::<ForceSensingButtonFeature>(&mut device)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(e.to_string()))?;
        feature
            .raw_call(function, args)
            .await
            .map_err(|e| HiddenDiagError::Hidpp(format!("{e:?}")))
    })
    .await
}
