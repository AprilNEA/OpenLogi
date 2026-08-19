use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::CreatableFeature,
    feature::FeatureType,
    feature::battery_status::BatteryStatusFeature,
    feature::device_information::{DeviceEntityType, DeviceInformationFeature},
    feature::feature_set::FeatureSetFeature,
    feature::unified_battery::UnifiedBatteryFeature,
};

use crate::channel::route::DeviceRoute;
use crate::reprog_controls::{self, CidFlags, CidInfo, ReprogControlsV4};
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Snapshot of one HID++ feature exposed by a device: protocol ID +
/// version. Returned by [`dump_features`] for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct FeatureEntry {
    /// HID++ feature ID.
    pub id: u16,
    /// Feature version reported by the device.
    pub version: u8,
    /// Obsolete / hidden / engineering flags the device advertises alongside
    /// the feature.
    pub typ: FeatureType,
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
/// feature IDs a given peripheral actually exposes (e.g. whether a mouse
/// speaks `0x2201 AdjustableDpi`, `0x2202 ExtendedAdjustableDpi`, or both —
/// `write::dpi` drives either).
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
                typ: info.typ,
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

/// Diagnostic read of the device's raw battery report — the unified `0x1004`
/// fields, or the legacy `0x1000` `discharge_level`/`next_level`/`status`. For
/// `openlogi diag battery`: surfaces exactly what the firmware reports so a
/// claim like "MX2S shows 0% while charging" can be confirmed against the wire
/// instead of guessed (the GUI only ever shows the mapped value).
pub async fn read_battery_raw(route: &DeviceRoute) -> Result<String, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;

        match open_feature::<UnifiedBatteryFeature>(&mut device).await {
            Ok(feature) => {
                let info = feature
                    .get_battery_info()
                    .await
                    .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
                return Ok(format!(
                    "0x1004 UnifiedBattery: percentage={} level={:?} status={:?}",
                    info.charging_percentage, info.level, info.status
                ));
            }
            Err(WriteError::FeatureUnsupported { .. }) => {}
            Err(e) => return Err(e),
        }

        match open_feature::<BatteryStatusFeature>(&mut device).await {
            Ok(feature) => {
                let info = feature
                    .get_battery_level_status()
                    .await
                    .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
                return Ok(format!(
                    "0x1000 BatteryStatus: discharge_level={} next_level={} status={:?}",
                    info.discharge_level, info.next_level, info.status
                ));
            }
            Err(WriteError::FeatureUnsupported { .. }) => {}
            Err(e) => return Err(e),
        }

        // Reached only when neither 0x1004 nor 0x1000 is present; report the
        // preferred feature rather than implying 0x1000 was specifically absent.
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x1004,
        })
    })
    .await
}

/// Snapshot of one firmware entity a device reports through HID++ `0x0003`
/// function 1. Returned by [`dump_firmware_entities`] so a device report can
/// name the exact firmware it is running.
#[derive(Debug, Clone)]
pub struct FirmwareEntityEntry {
    /// Index of the entity in the device's own table.
    pub index: u8,
    /// What the entity is: main application, bootloader, radio stack, and so
    /// on. `None` when the entity's record could not be parsed.
    pub kind: Option<DeviceEntityType>,
    /// Version string built from the entity's prefix, number, revision and
    /// build, e.g. `MPM17.00_B0008`. `None` when the record could not be
    /// parsed.
    pub version: Option<String>,
    /// USB or wireless product ID the entity runs under. A bootloader entity
    /// reports the PID the device enumerates as while in DFU mode.
    pub transport_pid: Option<u16>,
    /// Whether this is the entity currently running.
    pub active: bool,
    /// Why the entity's record could not be read, when it could not.
    pub error: Option<String>,
}

/// Read every firmware entity the device on `route` reports.
///
/// A device lists its main application firmware alongside its bootloader and,
/// on many models, a separate radio stack. `openlogi diag features` prints
/// them so a bug report names the firmware that produced the behaviour rather
/// than just the model.
///
/// A single unreadable entity does not fail the call. The device declares how
/// many entities it has, and one of them refusing to parse is itself worth
/// seeing, so the entry carries the error and the rest are still returned.
pub async fn dump_firmware_entities(
    route: &DeviceRoute,
) -> Result<Vec<FirmwareEntityEntry>, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<DeviceInformationFeature>(&mut device).await?;
        let info = feature.get_device_info().await.map_err(|e| {
            classify_hidpp_error(
                e,
                HidppOperation::DumpFeatures,
                DeviceInformationFeature::ID,
            )
        })?;

        let mut entries = Vec::with_capacity(usize::from(info.entity_count));
        for entity in 0..info.entity_count {
            entries.push(match feature.get_fw_info(entity).await {
                Ok(fw) => FirmwareEntityEntry {
                    index: entity,
                    kind: Some(fw.entity_type),
                    version: Some(format!(
                        "{}{:02}.{:02}_B{:04}",
                        fw.firmware_prefix, fw.firmware_number, fw.revision, fw.build
                    )),
                    transport_pid: Some(fw.transport_pid),
                    active: fw.active,
                    error: None,
                },
                Err(e) => FirmwareEntityEntry {
                    index: entity,
                    kind: None,
                    version: None,
                    transport_pid: None,
                    active: false,
                    error: Some(format!("{e:?}")),
                },
            });
        }
        Ok(entries)
    })
    .await
}
