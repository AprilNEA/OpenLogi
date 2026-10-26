//! HID++ `0x19c0 ForceSensingButton` reads and writes: the press force of a
//! force-sensing button such as the MX Master 4's Haptic Sense Panel.
//!
//! The button clicks at its firmware default with nothing written, so a
//! caller writes a force only when the user saved one. Every write reads the
//! button's range first and clamps into it: ranges differ from unit to unit,
//! and the protocol layer refuses a force outside the range the button
//! reports.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::force_sensing_button::{
        ForceButton, ForceButtonCapabilities, ForceButtonError, ForceButtonIndex,
        ForceSensingButtonFeature, ForceThreshold,
    },
    protocol::v20::Hidpp20Error,
};
use openlogi_core::hid::ForceSensingInfo;
use tracing::debug;

use super::{
    HidppFeatureErrorKind, HidppOperation, WriteError, classify_hidpp_error, open_feature,
};
use crate::SharedChannel;

/// Read the metadata of the button OpenLogi drives: the first, and on the
/// MX Master 4 the only one, its Haptic Sense Panel. The probe offers the
/// press force for this button too.
pub(crate) async fn panel_button(
    feature: &ForceSensingButtonFeature,
) -> Result<ForceButton, Hidpp20Error> {
    feature.get_button_info(ForceButtonIndex::from(0)).await
}

/// Whether software may change `button`'s press force.
pub(crate) fn force_button_is_changeable(button: &ForceButton) -> bool {
    button
        .capabilities()
        .contains(ForceButtonCapabilities::CHANGEABLE)
}

/// Read the press force and its range on an already-open
/// [`SharedChannel`].
pub async fn get_force_sensing_on(shared: &SharedChannel) -> Result<ForceSensingInfo, WriteError> {
    let operation = HidppOperation::ReadForceSensing;
    let index = shared.device_index();
    let feature = open_force_feature(shared.channel(), index).await?;
    let button = read_button(&feature, operation).await?;
    let current = read_current(&button, operation).await?;
    Ok(info(&button, current))
}

/// Write `force`, clamped into the range the device reports, on an
/// already-open [`SharedChannel`], and return what the device reports
/// afterwards.
///
/// A device that does not let software change its force is refused with
/// [`WriteError::ForceSensingUnchangeable`] and nothing is written. A force
/// the device already has is not written again. A failed read-back is
/// returned as an error, so only a force the device confirmed is ever
/// saved.
pub async fn set_force_sensing_on(
    shared: &SharedChannel,
    force: u16,
) -> Result<ForceSensingInfo, WriteError> {
    let operation = HidppOperation::WriteForceSensing;
    let index = shared.device_index();
    let feature = open_force_feature(shared.channel(), index).await?;
    let button = read_button(&feature, operation).await?;
    if !force_button_is_changeable(&button) {
        return Err(WriteError::ForceSensingUnchangeable);
    }
    let before = info(&button, read_current(&button, operation).await?);
    let requested = force;
    let force = before.clamp(requested);
    if force != requested {
        debug!(
            index,
            requested,
            clamped = force,
            min = before.min,
            max = before.max,
            "press force outside the device's range"
        );
    }
    if before.current == force {
        debug!(index, force, "press force already set");
        return Ok(before);
    }
    button
        .set_button_config(ForceThreshold::from(force))
        .await
        .map_err(|error| match error {
            ForceButtonError::ReadOnly => WriteError::ForceSensingUnchangeable,
            // Unreachable after the clamp above; reported as the device
            // would report it.
            ForceButtonError::OutOfRange => WriteError::HidppFeature {
                operation,
                feature_hex: ForceSensingButtonFeature::ID,
                kind: HidppFeatureErrorKind::OutOfRange,
            },
            ForceButtonError::Protocol(error) => {
                classify_hidpp_error(error, operation, ForceSensingButtonFeature::ID)
            }
        })?;
    let actual = read_current(&button, operation).await?;
    if actual != force {
        return Err(WriteError::ForceSensingNotApplied {
            requested: force,
            actual,
        });
    }
    debug!(index, force, "press force written");
    Ok(ForceSensingInfo {
        current: actual,
        ..before
    })
}

async fn open_force_feature(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<ForceSensingButtonFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<ForceSensingButtonFeature>(&mut device).await
}

async fn read_button(
    feature: &ForceSensingButtonFeature,
    operation: HidppOperation,
) -> Result<ForceButton, WriteError> {
    panel_button(feature)
        .await
        .map_err(|error| classify_hidpp_error(error, operation, ForceSensingButtonFeature::ID))
}

async fn read_current(button: &ForceButton, operation: HidppOperation) -> Result<u16, WriteError> {
    button
        .get_button_config()
        .await
        .map(u16::from)
        .map_err(|error| classify_hidpp_error(error, operation, ForceSensingButtonFeature::ID))
}

/// The IPC snapshot of `button`'s metadata with `current`, its force now.
fn info(button: &ForceButton, current: u16) -> ForceSensingInfo {
    let range = button.range();
    ForceSensingInfo {
        changeable: force_button_is_changeable(button),
        default: button.default_threshold().into(),
        min: (*range.start()).into(),
        max: (*range.end()).into(),
        current,
    }
}

#[cfg(test)]
mod tests;
