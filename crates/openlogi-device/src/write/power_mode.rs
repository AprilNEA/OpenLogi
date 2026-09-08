//! HID++ `0x8090 ModeStatus` — the performance / endurance power mode on
//! G-series mice (G305 and friends).
//!
//! The device persists the mode across power cycles in its own memory, so
//! there is no host-side persistence: callers read the device and write the
//! device, nothing else.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature,
        mode_status::{ModeStatus0, ModeStatusCapabilities, ModeStatusFeature},
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use openlogi_core::hid::mode_status::{PowerMode, PowerModeState};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Read the current power mode plus the switch capabilities of the device
/// `route` reaches.
///
/// `FeatureUnsupported` when the device does not expose HID++ `0x8090`.
pub async fn get_power_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<PowerModeState, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_power_mode_on_channel(&channel, index).await
    })
    .await
}

pub(super) async fn get_power_mode_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<PowerModeState, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ModeStatusFeature>(&mut device).await?;
    let status = feature.get_mode_status().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::ReadPowerMode, ModeStatusFeature::ID)
    })?;
    let capabilities = feature.get_device_config().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::ReadPowerMode, ModeStatusFeature::ID)
    })?;
    Ok(PowerModeState {
        mode: if status.status0.contains(ModeStatus0::PERFORMANCE) {
            PowerMode::Performance
        } else {
            PowerMode::Endurance
        },
        software_switch: capabilities.contains(ModeStatusCapabilities::SOFTWARE_SWITCH),
        hardware_switch: capabilities.contains(ModeStatusCapabilities::HARDWARE_SWITCH),
    })
}

/// Write a new power mode to the device `route` reaches. The device persists
/// the mode across power cycles itself, so nothing is stored host-side.
///
/// Only `changed_mask0` is written: a G305 answers HID++ `InvalidArgument` to
/// any set that touches `changed_mask1`.
///
/// `FeatureUnsupported` when the device does not expose HID++ `0x8090`.
pub async fn set_power_mode(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    mode: PowerMode,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_power_mode_on_channel(&channel, index, mode).await
    })
    .await
}

pub(super) async fn set_power_mode_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    mode: PowerMode,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ModeStatusFeature>(&mut device).await?;
    feature
        .set_performance_mode(matches!(mode, PowerMode::Performance))
        .await
        .map_err(|e| {
            classify_hidpp_error(e, HidppOperation::WritePowerMode, ModeStatusFeature::ID)
        })?;
    debug!(index, ?mode, "wrote power mode");
    Ok(())
}

/// Read the power mode and switch capabilities on an already-open
/// [`SharedChannel`].
pub async fn get_power_mode_on(shared: &SharedChannel) -> Result<PowerModeState, WriteError> {
    get_power_mode_on_channel(shared.channel(), shared.device_index()).await
}

/// Write a new power mode on an already-open [`SharedChannel`] — the fast
/// path that skips enumeration and channel setup.
pub async fn set_power_mode_on(shared: &SharedChannel, mode: PowerMode) -> Result<(), WriteError> {
    set_power_mode_on_channel(shared.channel(), shared.device_index(), mode).await
}
