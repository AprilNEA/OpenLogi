//! A software-initiated switch of one device: the settings app's Switch
//! button and the Easy-Switch actions of a device with no linked keyboard.
//!
//! It moves the device to another slot with `0x1814 ChangeHost`, as the
//! device's own channel key does. `setCurrentHost` is fire-and-forget: the
//! device acts on it by leaving this computer, so nothing is read back. A
//! slot the device reports as empty is refused first, with the same check
//! the session applies before a keyboard switch, because a device sent there
//! drops off and does not come back until someone presses its channel key.

use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{CreatableFeature as _, change_host::ChangeHostFeature},
};
use tracing::debug;

use super::host_slot_is_empty;
use crate::SharedChannel;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, open_feature};

/// Move the device on an already-open [`SharedChannel`] to Easy-Switch
/// `slot` (zero-based) and answer whether it was sent away: `false` when the
/// device already uses `slot`. A device without `0x1814` fails with
/// [`WriteError::FeatureUnsupported`]; a slot the device does not have, or
/// reports empty, with [`WriteError::HostSlotEmpty`] before anything is sent.
/// Once the request is out, the device leaves this computer.
pub async fn switch_easy_switch_host_on(
    shared: &SharedChannel,
    slot: u8,
) -> Result<bool, WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<ChangeHostFeature>(&mut device).await?;
    let classify = |e| classify_hidpp_error(e, HidppOperation::SwitchHost, ChangeHostFeature::ID);
    let info = feature.get_host_info().await.map_err(classify)?;
    if slot >= info.host_count {
        return Err(WriteError::HostSlotEmpty { slot });
    }
    if info.current_host == slot {
        debug!(index, slot, "device already uses the requested host");
        return Ok(false);
    }
    if host_slot_is_empty(&mut device, slot).await {
        return Err(WriteError::HostSlotEmpty { slot });
    }
    feature.set_current_host(slot).await.map_err(classify)?;
    debug!(index, slot, "host switch sent");
    Ok(true)
}
