//! ChangeHost (`0x1814`) current-host read for the Flow tab.

use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{CreatableFeature, change_host::ChangeHostFeature},
};

use crate::SharedChannel;

use super::{HidppOperation, WriteError, classify_hidpp_error};

pub use openlogi_core::hid::host::HostInfo;

/// Read which host slot the device is on right now, on an already-open
/// [`SharedChannel`]. The GUI calls this over IPC while a Flow tab is open;
/// switching itself goes through the session layer's `switch_linked_hosts`.
pub async fn get_host_info_on(shared: &SharedChannel) -> Result<HostInfo, WriteError> {
    let channel = shared.channel();
    let index = shared.device_index();
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature_hex = ChangeHostFeature::ID;
    let feature = device
        .root()
        .get_feature(feature_hex)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, feature_hex))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex })?;
    let change_host = device.add_feature::<ChangeHostFeature>(feature.index);
    let state = change_host
        .get_host_info()
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ReadHostInfo, feature_hex))?;
    Ok(HostInfo {
        current_host: state.current_host,
        host_count: state.host_count,
    })
}
