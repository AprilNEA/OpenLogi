use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::FeatureType,
    feature::battery_status::BatteryStatusFeature,
    feature::device_information::{
        DeviceEntityFirmwareInfo, DeviceEntityType, DeviceInformationFeature,
    },
    feature::feature_set::FeatureSetFeature,
    feature::hosts_info::{HostBusType, HostIndex, HostSlotStatus, HostsInfoFeature},
    feature::unified_battery::UnifiedBatteryFeature,
    protocol::v20::Hidpp20Error,
};

use crate::backend::HidBackend;
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

/// Pairing state of one Easy-Switch host slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticHostSlotStatus {
    /// No host is paired in this slot.
    Empty,
    /// A host is paired in this slot.
    Paired,
}

/// Wireless transport recorded for one Easy-Switch host slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticHostBus {
    /// The device did not identify the transport.
    Undefined,
    /// Logitech eQuad / Unifying transport.
    Equad,
    /// Wired USB transport.
    Usb,
    /// Bluetooth Classic transport.
    Bluetooth,
    /// Bluetooth Low Energy transport.
    BluetoothLowEnergy,
    /// Bluetooth Low Energy Pro / Logi Bolt transport.
    Bolt,
}

/// Read-only snapshot of one Easy-Switch host slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticHostSlot {
    /// Zero-based slot index used by HID++ `ChangeHost`.
    pub index: u8,
    /// Whether a host is paired in the slot.
    pub status: DiagnosticHostSlotStatus,
    /// Transport associated with the pairing.
    pub bus: DiagnosticHostBus,
}

/// Read-only snapshot of a device's Easy-Switch host table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticHosts {
    /// Zero-based active host slot, when the device returned a concrete slot.
    pub current_host: Option<u8>,
    /// Every slot reported by the device.
    pub slots: Vec<DiagnosticHostSlot>,
}

impl TryFrom<HostSlotStatus> for DiagnosticHostSlotStatus {
    type Error = WriteError;

    fn try_from(status: HostSlotStatus) -> Result<Self, Self::Error> {
        match status {
            HostSlotStatus::Empty => Ok(Self::Empty),
            HostSlotStatus::Paired => Ok(Self::Paired),
            _ => Err(unsupported_hosts_response()),
        }
    }
}

impl TryFrom<HostBusType> for DiagnosticHostBus {
    type Error = WriteError;

    fn try_from(bus: HostBusType) -> Result<Self, Self::Error> {
        match bus {
            HostBusType::Undefined => Ok(Self::Undefined),
            HostBusType::Equad => Ok(Self::Equad),
            HostBusType::Usb => Ok(Self::Usb),
            HostBusType::Bt => Ok(Self::Bluetooth),
            HostBusType::Ble => Ok(Self::BluetoothLowEnergy),
            HostBusType::BlePro => Ok(Self::Bolt),
            _ => Err(unsupported_hosts_response()),
        }
    }
}

fn current_host_slot(host: HostIndex) -> Result<Option<u8>, WriteError> {
    match host {
        HostIndex::Current => Ok(None),
        HostIndex::Slot(slot) => Ok(Some(slot)),
        _ => Err(unsupported_hosts_response()),
    }
}

fn unsupported_hosts_response() -> WriteError {
    WriteError::UnsupportedResponse {
        operation: HidppOperation::DumpFeatures,
        feature_hex: HostsInfoFeature::ID,
    }
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
pub async fn dump_features(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Vec<FeatureEntry>, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
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
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Vec<ReprogControlEntry>, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
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

/// Read the Easy-Switch host table without changing the active host.
pub async fn dump_hosts(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<DiagnosticHosts, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let hosts = open_feature::<HostsInfoFeature>(&mut device).await?;
        let feature_info = hosts.get_feature_info().await.map_err(|error| {
            classify_hidpp_error(error, HidppOperation::DumpFeatures, HostsInfoFeature::ID)
        })?;
        let current_host = current_host_slot(feature_info.current_host)?;
        let mut slots = Vec::with_capacity(usize::from(feature_info.host_count));
        for slot in 0..feature_info.host_count {
            let info = hosts
                .get_host_info(HostIndex::Slot(slot))
                .await
                .map_err(|error| {
                    classify_hidpp_error(error, HidppOperation::DumpFeatures, HostsInfoFeature::ID)
                })?;
            slots.push(DiagnosticHostSlot {
                index: slot,
                status: info.status.try_into()?,
                bus: info.bus_type.try_into()?,
            });
        }
        Ok(DiagnosticHosts {
            current_host,
            slots,
        })
    })
    .await
}

/// Diagnostic read of the device's raw battery report — the unified `0x1004`
/// fields, or the legacy `0x1000` `discharge_level`/`next_level`/`status`. For
/// `openlogi diag battery`: surfaces exactly what the firmware reports so a
/// claim like "MX2S shows 0% while charging" can be confirmed against the wire
/// instead of guessed (the GUI only ever shows the mapped value).
pub async fn read_battery_raw(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<String, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
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

/// Firmware fields for one entity whose record the device answered and this
/// parser decoded.
///
/// Owned, constructible data converted from `hidpp`'s
/// `DeviceEntityFirmwareInfo`, the same way [`ReprogControlEntry`] is
/// converted from `CidInfo`: consumers get the structured record and decide
/// how to render it, rather than being handed a pre-formatted string with the
/// rest of the fields dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareEntityInfo {
    /// What the entity is: main application, bootloader, radio stack, and so
    /// on.
    pub kind: DeviceEntityType,
    /// Three-letter prefix of the firmware name, e.g. `MPM`.
    pub prefix: String,
    /// Firmware number, BCD-decoded by the protocol layer.
    pub number: u8,
    /// Firmware revision, BCD-decoded by the protocol layer.
    pub revision: u8,
    /// Firmware build, BCD-decoded by the protocol layer.
    pub build: u16,
    /// Whether this is the entity currently running.
    pub active: bool,
    /// USB or wireless product ID the entity runs under. A bootloader entity
    /// reports the PID the device enumerates as while in DFU mode; only the
    /// active entity is required to report a real value, so an inactive one
    /// may be zero.
    pub transport_pid: u16,
    /// Optional extra versioning bytes. Device-specific and usually all zero,
    /// carried verbatim because a device that does populate them is exactly
    /// the device a report is being collected for.
    pub extra_version: [u8; 5],
}

impl From<DeviceEntityFirmwareInfo> for FirmwareEntityInfo {
    fn from(info: DeviceEntityFirmwareInfo) -> Self {
        Self {
            kind: info.entity_type,
            prefix: info.firmware_prefix,
            number: info.firmware_number,
            revision: info.revision,
            build: info.build,
            active: info.active,
            transport_pid: info.transport_pid,
            extra_version: info.extra_version,
        }
    }
}

/// One firmware entity a device reports through HID++ `0x0003` function 1.
/// Returned by [`dump_firmware_entities`] so a device report can name the
/// exact firmware it is running.
///
/// There are two states and only two: the device answered with a record that
/// decoded, or it did not. An enum makes "a version with no kind" and "an
/// error alongside a version" unrepresentable rather than merely unreachable.
#[derive(Debug, Clone)]
pub enum FirmwareEntity {
    /// The entity's record was read and decoded.
    Readable {
        /// Index of the entity in the device's own table.
        index: u8,
        /// The decoded firmware record.
        info: FirmwareEntityInfo,
    },
    /// The device declared the entity, but its record could not be read.
    ///
    /// Reported rather than dropped: omitting the row would claim the device
    /// has fewer firmware images than it says it has, and a device that cannot
    /// describe one of its own images is what a bug report needs to say.
    Unreadable {
        /// Index of the entity in the device's own table.
        index: u8,
        /// Why the record could not be read.
        error: WriteError,
    },
}

/// Read every firmware entity the device on `route` reports.
///
/// A device lists its main application firmware alongside its bootloader and,
/// on many models, a separate radio stack. `openlogi diag features` prints
/// them so a bug report names the firmware that produced the behaviour rather
/// than just the model.
///
/// A single entity the *device* declined or answered unparseably does not fail
/// the call — see [`FirmwareEntity::Unreadable`]. A channel failure does: the
/// route is gone, not the entity.
pub async fn dump_firmware_entities(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Vec<FirmwareEntity>, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        dump_firmware_entities_on_channel(&channel, index).await
    })
    .await
}

/// [`dump_firmware_entities`] against an already-open channel, the shape the
/// tests drive a scripted device through.
pub(crate) async fn dump_firmware_entities_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Vec<FirmwareEntity>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
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
        match feature.get_fw_info(entity).await {
            Ok(fw) => entries.push(FirmwareEntity::Readable {
                index: entity,
                info: fw.into(),
            }),
            // The device answered about *this* entity and the answer was no:
            // it refused the read, or it sent a record this parser cannot
            // decode (a G502's radio stack reports a build field that is not
            // valid BCD). The rest of the table is still worth reading.
            Err(e @ (Hidpp20Error::Feature(_) | Hidpp20Error::UnsupportedResponse)) => {
                entries.push(FirmwareEntity::Unreadable {
                    index: entity,
                    error: classify_hidpp_error(
                        e,
                        HidppOperation::DumpFeatures,
                        DeviceInformationFeature::ID,
                    ),
                });
            }
            // A channel failure says nothing about the entity — the route
            // disappeared. Carrying on would spend a timeout per remaining
            // entity and then print malformed-firmware rows for a disconnect.
            Err(e) => {
                return Err(classify_hidpp_error(
                    e,
                    HidppOperation::DumpFeatures,
                    DeviceInformationFeature::ID,
                ));
            }
        }
    }
    Ok(entries)
}
