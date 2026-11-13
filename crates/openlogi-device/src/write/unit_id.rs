//! The unit ID a device reports through HID++ `0x0003` `DeviceInformation`,
//! which tells apart two devices on one route: two devices paired into one
//! receiver slot in turn, or two units of one model on a direct route.

use std::sync::Arc;

use hidpp::channel::ChannelError;
use hidpp::device::Device;
use hidpp::feature::CreatableFeature;
use hidpp::feature::device_information::DeviceInformationFeature;
use hidpp::protocol::v20::Hidpp20Error;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature};
use crate::SharedChannel;

/// [`get_unit_id_on`] for a `device` already open.
pub(crate) async fn unit_id(
    device: &mut Device,
    operation: HidppOperation,
) -> Result<Option<[u8; 4]>, WriteError> {
    let feature = match open_feature::<DeviceInformationFeature>(device).await {
        Ok(feature) => feature,
        Err(WriteError::FeatureUnsupported { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let info = feature
        .get_device_info()
        .await
        .map_err(|error| match error {
            Hidpp20Error::Channel(ChannelError::Timeout) => {
                WriteError::RequestTimedOut { operation }
            }
            other => classify_hidpp_error(other, operation, DeviceInformationFeature::ID),
        })?;
    Ok(Some(info.unit_id))
}

/// The unit ID the device on `shared` reports, or `None` when it has no
/// `DeviceInformation` feature, which a write bound to one device reads to
/// confirm that device is still the one on the channel. The read is part of
/// `operation`: a timeout is its [`WriteError::RequestTimedOut`], and any
/// other failure its error on `0x0003`.
pub async fn get_unit_id_on(
    shared: &SharedChannel,
    operation: HidppOperation,
) -> Result<Option<[u8; 4]>, WriteError> {
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(shared.channel()), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    unit_id(&mut device, operation).await
}
