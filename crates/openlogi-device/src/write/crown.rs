//! HID++ `0x4600 Crown` reads and writes for the Craft's rotary crown.
//!
//! No config schema or IPC wiring exists for this yet — these are the
//! device-layer primitives a later config/IPC layer will call.

use std::sync::Arc;

use hidpp::device::Device;
use hidpp::feature::CreatableFeature;
use hidpp::feature::crown::{CrownFeature, CrownInfo, CrownMode, SetCrownMode};

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

/// Write the crown's mode and read it back to verify.
///
/// `SetCrownMode`'s `None` fields mean "leave unchanged" on the wire, and the
/// device's `SetMode` response only echoes the request — it carries no
/// statement about the resulting mode ([`CrownFeature::set_mode`]'s own
/// rustdoc) — so verification re-reads with [`CrownFeature::get_mode`] and
/// checks only the fields this call actually requested.
pub async fn set_crown_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    mode: SetCrownMode,
) -> Result<CrownMode, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<CrownFeature>(&mut device).await?;
        feature.set_mode(mode).await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::WriteCrownMode, CrownFeature::ID)
        })?;
        let read_back = feature.get_mode().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::ReadCrownMode, CrownFeature::ID)
        })?;
        validate_applied(mode, read_back)?;
        Ok(read_back)
    })
    .await
}

/// Checks that every field `requested` actually asked to change landed in
/// `actual`. A `None` field requested no change and is not checked, so a
/// concurrent change to it by another host-side writer is not flagged here.
fn validate_applied(requested: SetCrownMode, actual: CrownMode) -> Result<(), WriteError> {
    let mismatched = requested.diverting.is_some_and(|v| v != actual.diverting)
        || requested
            .ratchet_mode
            .is_some_and(|v| v != actual.ratchet_mode)
        || requested
            .rotation_timeout
            .is_some_and(|v| v.get() != actual.rotation_timeout)
        || requested
            .short_long_timeout
            .is_some_and(|v| v.get() != actual.short_long_timeout)
        || requested
            .double_tap_speed
            .is_some_and(|v| v.get() != actual.double_tap_speed);
    if mismatched {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::WriteCrownMode,
            feature_hex: CrownFeature::ID,
        });
    }
    Ok(())
}
