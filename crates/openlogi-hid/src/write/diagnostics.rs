use std::sync::Arc;

use hidpp::{device::Device, feature::CreatableFeature, feature::feature_set::FeatureSetFeature};

use crate::reprog_controls::{self, CidFlags, CidInfo, ReprogControlsV4};
use crate::route::DeviceRoute;
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, with_route};

/// Snapshot of one HID++ feature exposed by a device: protocol ID +
/// version. Returned by [`dump_features`] for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct FeatureEntry {
    /// HID++ feature ID.
    pub id: u16,
    /// Feature version reported by the device.
    pub version: u8,
}

/// Snapshot of one HID++ `0x1b04` reprogrammable control. Returned by
/// [`dump_reprog_controls`] for diagnostics so new device controls can be
/// identified before OpenLogi maps them to a first-class button.
#[derive(Debug, Clone, Copy)]
pub struct ReprogControlEntry {
    /// HID++ control ID.
    pub cid: u16,
    /// Default task ID assigned to the control.
    pub task_id: u16,
    /// Capability and classification flags for the control.
    pub flags: CidFlags,
}

impl From<CidInfo> for ReprogControlEntry {
    fn from(info: CidInfo) -> Self {
        Self {
            cid: info.cid.into(),
            task_id: info.task_id.0,
            flags: info.flags,
        }
    }
}

/// Enumerate every HID++ feature the device on `route` reports — used by
/// `openlogi diag features` to confirm which DPI / SmartShift / etc.
/// feature IDs a given peripheral actually exposes (e.g. some mice use
/// `0x2202 ExtendedAdjustableDpi` instead of `0x2201 AdjustableDpi`).
pub async fn dump_features(route: &DeviceRoute) -> Result<Vec<FeatureEntry>, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        // The root feature exposes the FeatureSet (0x0001) at a fixed
        // address; we look it up directly rather than going through
        // `enumerate_features` so the iteration is observable.
        let feature_set_info = device
            .root()
            .get_feature(FeatureSetFeature::ID)
            .await
            .map_err(|e| {
                classify_hidpp_error(e, HidppOperation::DumpFeatures, FeatureSetFeature::ID)
            })?
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: FeatureSetFeature::ID,
            })?;
        let feature_set = device.add_feature::<FeatureSetFeature>(feature_set_info.index);
        let count = feature_set.count().await.map_err(|e| {
            classify_hidpp_error(e, HidppOperation::DumpFeatures, FeatureSetFeature::ID)
        })?;
        let mut entries = Vec::with_capacity(usize::from(count));
        for i in 0..=count {
            let info = feature_set.get_feature(i).await.map_err(|e| {
                classify_hidpp_error(e, HidppOperation::DumpFeatures, FeatureSetFeature::ID)
            })?;
            entries.push(FeatureEntry {
                id: info.id,
                version: info.version,
            });
        }
        Ok(entries)
    })
    .await
}

/// Enumerate the device's HID++ `0x1b04` reprogrammable controls. This is a
/// diagnostics-only probe used to discover controls for newly released devices.
/// For example, MX Master 4 has both a Gesture Button and a separate Haptic
/// Sense Panel in the thumb area; this probe lets us identify the panel's CID
/// and capabilities before wiring it into the capture/remapping model.
pub async fn dump_reprog_controls(
    route: &DeviceRoute,
) -> Result<Vec<ReprogControlEntry>, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let info = device
            .root()
            .get_feature(reprog_controls::FEATURE_ID)
            .await
            .map_err(|e| {
                classify_hidpp_error(e, HidppOperation::DumpFeatures, reprog_controls::FEATURE_ID)
            })?
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: reprog_controls::FEATURE_ID,
            })?;
        let rc = ReprogControlsV4::new(Arc::clone(&channel), index, info.index);
        let count = rc.get_count().await.map_err(|e| {
            classify_hidpp_error(e, HidppOperation::DumpFeatures, reprog_controls::FEATURE_ID)
        })?;
        let mut entries = Vec::with_capacity(usize::from(count));
        for i in 0..count {
            let control = rc.get_cid_info(i).await.map_err(|e| {
                classify_hidpp_error(e, HidppOperation::DumpFeatures, reprog_controls::FEATURE_ID)
            })?;
            entries.push(control.into());
        }
        Ok(entries)
    })
    .await
}
