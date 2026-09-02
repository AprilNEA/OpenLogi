//! HID++ `0x4600 Crown` reads — read-only smoke test for the Craft's rotary
//! crown before any write path, config schema, or IPC wiring exists for it.

use std::sync::Arc;

use hidpp::device::Device;
use hidpp::feature::CreatableFeature;
use hidpp::feature::crown::{CrownFeature, CrownInfo, CrownMode};

use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Read the crown's capabilities and slot/ratchet counts.
pub async fn get_crown_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<CrownInfo, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<CrownFeature>(&mut device).await?;
        feature.get_info().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::ReadCrownInfo, CrownFeature::ID)
        })
    })
    .await
}

/// Read the crown's current mode (reporting target, ratchet, timeouts).
pub async fn get_crown_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<CrownMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<CrownFeature>(&mut device).await?;
        feature.get_mode().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::ReadCrownMode, CrownFeature::ID)
        })
    })
    .await
}
