use std::sync::Arc;

use hidpp::{
    channel::{HidppChannel, LONG_REPORT_LENGTH},
    device::Device,
    feature::CreatableFeature,
    feature::FeatureType,
    feature::adc_measurement::AdcMeasurementFeature,
    feature::battery_status::BatteryStatusFeature,
    feature::battery_voltage::BatteryVoltageFeature,
    feature::device_information::{
        DeviceEntityFirmwareInfo, DeviceEntityType, DeviceInformationFeature,
    },
    feature::feature_set::FeatureSetFeature,
    feature::root::RootFeature,
    feature::unified_battery::{BatteryInfo as UnifiedBatteryInfo, UnifiedBatteryFeature},
    nibble::U4,
    protocol::v20::{self, ErrorType, Hidpp20Error},
};

use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;
use crate::reprog_controls::{self, CidFlags, CidInfo, ReprogControlsV4};
use crate::write::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Snapshot of one HID++ feature exposed by a device: protocol ID +
/// version. Returned by [`dump_features`] for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureEntry {
    /// Position of the entry in the device's feature table.
    pub index: u8,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
///
/// Index `0` is the root feature. Most devices list it in their FeatureSet
/// table, and the dump keeps that row. [`FeatureSetFeature`] reserves the
/// index, though, and some firmware refuses it: the Yeti X answers
/// `InvalidArgument` (#786). A refused root row is left out, so each
/// [`FeatureEntry::index`] stays the device's own index. Any other error
/// fails the dump.
pub async fn dump_features(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Vec<FeatureEntry>, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        dump_features_on_channel(&channel, index).await
    })
    .await
}

/// [`dump_features`] against an already-open channel, the shape the tests
/// drive a scripted device through.
pub(crate) async fn dump_features_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Vec<FeatureEntry>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    // The root feature exposes the FeatureSet (0x0001) at a fixed
    // address; we look it up directly rather than going through
    // `enumerate_features` so the iteration is observable.
    let feature_set_info = device
        .root()
        .get_feature(FeatureSetFeature::ID)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::DumpFeatures, FeatureSetFeature::ID))?
        .ok_or(WriteError::FeatureUnsupported {
            feature_hex: FeatureSetFeature::ID,
        })?;
    let feature_set = device.add_feature::<FeatureSetFeature>(feature_set_info.index);
    let count = feature_set.count().await.map_err(|e| {
        classify_hidpp_error(e, HidppOperation::DumpFeatures, FeatureSetFeature::ID)
    })?;
    let mut entries = Vec::with_capacity(usize::from(count) + 1);
    for i in 0..=count {
        let info = match feature_set.get_feature(i).await {
            Ok(info) => info,
            Err(Hidpp20Error::Feature(ErrorType::InvalidArgument)) if i == 0 => continue,
            Err(e) => {
                return Err(classify_hidpp_error(
                    e,
                    HidppOperation::DumpFeatures,
                    FeatureSetFeature::ID,
                ));
            }
        };
        entries.push(FeatureEntry {
            index: i,
            id: info.id,
            version: info.version,
            typ: info.typ,
        });
    }
    Ok(entries)
}

/// Parameter bytes one long HID++ 2.0 report carries, and the length of
/// every [`RawFeatureReply::Payload`].
pub const RAW_FEATURE_PARAMS: usize = LONG_REPORT_LENGTH - 4;

/// One raw call to a HID++ 2.0 feature, for [`raw_feature_calls`]: a
/// function number and the parameters of a long report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawFeatureCall {
    function: U4,
    params: [u8; RAW_FEATURE_PARAMS],
}

impl RawFeatureCall {
    /// The call of `function` with `params`, or `None` past function 15: a
    /// HID++ 2.0 function number is four bits, and a larger one would go out
    /// as a different function.
    #[must_use]
    pub fn new(function: u8, params: [u8; RAW_FEATURE_PARAMS]) -> Option<Self> {
        if function > 0x0f {
            return None;
        }
        Some(Self {
            function: U4::from_lo(function),
            params,
        })
    }

    /// The function number, 0–15.
    #[must_use]
    pub fn function(&self) -> u8 {
        self.function.to_lo()
    }

    /// The parameter bytes, as sent.
    #[must_use]
    pub fn params(&self) -> &[u8; RAW_FEATURE_PARAMS] {
        &self.params
    }
}

/// What one raw call to a HID++ 2.0 feature got back. For
/// [`raw_feature_calls`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFeatureReply {
    /// The response payload; a short response is zero-padded to
    /// [`RAW_FEATURE_PARAMS`] bytes.
    Payload([u8; RAW_FEATURE_PARAMS]),
    /// The error the feature answered with.
    Error(ErrorType),
}

/// What [`raw_feature_calls`] got back from one feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFeatureAnswers {
    /// The feature's index in the device's feature table.
    pub index: u8,
    /// One reply per call, in call order, up to the first call that got
    /// none.
    pub replies: Vec<RawFeatureReply>,
    /// Why the call after the last reply got no answer; the calls after it
    /// were not sent. `None` when every call was answered.
    pub failure: Option<WriteError>,
}

/// Call feature `feature_id` of the device on `route` once for each of
/// `calls`, in order, over one channel, and return what they got back.
/// `None` when the device has no such feature. Each call goes out as a long
/// report.
///
/// A call that gets no answer stops the rest, and the answers before it are
/// kept: earlier calls may already have changed the device.
///
/// For `openlogi diag hidpp`, to learn an undocumented feature's functions
/// from their answers. Nothing checks what a function does: a call can write
/// as easily as read, so the caller picks the functions.
pub async fn raw_feature_calls(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    feature_id: u16,
    calls: &[RawFeatureCall],
) -> Result<Option<RawFeatureAnswers>, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        raw_feature_calls_on_channel(&channel, index, feature_id, calls).await
    })
    .await
}

/// [`raw_feature_calls`] against an already-open channel, the shape the
/// tests drive a scripted device through.
pub(crate) async fn raw_feature_calls_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    feature_id: u16,
    calls: &[RawFeatureCall],
) -> Result<Option<RawFeatureAnswers>, WriteError> {
    let device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    // The root answers a lookup of itself with index 0, which reads as
    // absent, but it lives at index 0 on every HID++ 2.0 device.
    let feature_index = if feature_id == RootFeature::ID {
        0
    } else {
        let Some(info) = device
            .root()
            .get_feature(feature_id)
            .await
            .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, feature_id))?
        else {
            return Ok(None);
        };
        info.index
    };
    let mut answers = RawFeatureAnswers {
        index: feature_index,
        replies: Vec::with_capacity(calls.len()),
        failure: None,
    };
    for call in calls {
        let header = v20::MessageHeader {
            device_index: index,
            feature_index,
            function_id: call.function,
            software_id: channel.get_sw_id(),
        };
        let reply = match channel
            .send_v20(v20::Message::Long(header, call.params))
            .await
        {
            Ok(response) => RawFeatureReply::Payload(response.extend_payload()),
            Err(Hidpp20Error::Feature(error)) => RawFeatureReply::Error(error),
            Err(error) => {
                answers.failure = Some(WriteError::Hidpp(format!("{error:?}")));
                break;
            }
        };
        answers.replies.push(reply);
    }
    Ok(Some(answers))
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

/// The battery features [`read_battery_raw`] reads, in the order it tries
/// them: unified `0x1004`, legacy `0x1000`, voltage `0x1001`, then the `0x1F20`
/// ADC. A device exposing none of them has no battery this command can read.
pub const RAW_BATTERY_FEATURES: [u16; 4] = [
    UnifiedBatteryFeature::ID,
    BatteryStatusFeature::ID,
    BatteryVoltageFeature::ID,
    AdcMeasurementFeature::ID,
];

/// Diagnostic read of the device's raw battery report — the unified `0x1004`
/// fields, the legacy `0x1000` `discharge_level`/`next_level`/`status`, the
/// `0x1001` measured voltage and charging flags, or the `0x1F20` ADC voltage
/// and charging state. For
/// `openlogi diag battery`: surfaces exactly what the firmware reports so a
/// claim like "MX2S shows 0% while charging" can be confirmed against the wire
/// instead of guessed (the GUI only ever shows the mapped value).
///
/// Fails with [`WriteError::FeatureUnsupported`] naming `0x1004`, the preferred
/// feature, when the device exposes none of [`RAW_BATTERY_FEATURES`].
pub async fn read_battery_raw(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<String, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        read_battery_raw_on_channel(&channel, index).await
    })
    .await
}

/// [`read_battery_raw`] against an already-open channel, the shape the tests
/// drive a scripted device through.
pub(crate) async fn read_battery_raw_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<String, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;

    match super::open_feature_with_version::<UnifiedBatteryFeature>(&mut device).await {
        Ok((feature, version)) => {
            let info = feature
                .get_battery_info()
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
            return Ok(unified_battery_line(version, info));
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

    // The measured voltage, not the percentage the inventory interpolates from
    // it: checking that estimate against the wire is what this command is for.
    match open_feature::<BatteryVoltageFeature>(&mut device).await {
        Ok(feature) => {
            let info = feature
                .get_battery_info()
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
            return Ok(format!(
                "0x1001 BatteryVoltage: voltage_mv={} critical={} status={:?}",
                info.voltage_mv, info.critical, info.status
            ));
        }
        Err(WriteError::FeatureUnsupported { .. }) => {}
        Err(e) => return Err(e),
    }

    match open_feature::<AdcMeasurementFeature>(&mut device).await {
        Ok(feature) => {
            let measurement = feature
                .get_adc_measurement()
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
            return Ok(format!("0x1F20 AdcMeasurement: {measurement:?}"));
        }
        Err(WriteError::FeatureUnsupported { .. }) => {}
        Err(e) => return Err(e),
    }

    // Reached only when none of the read features is present; report the
    // preferred feature rather than implying 0x1F20 was specifically absent.
    Err(WriteError::FeatureUnsupported {
        feature_hex: UnifiedBatteryFeature::ID,
    })
}

/// The [`read_battery_raw`] line for a `0x1004` status at feature `version`.
/// The fields after the charging status print as raw bytes; a field that the
/// version or a short reply leaves out is not printed.
fn unified_battery_line(version: u8, info: UnifiedBatteryInfo) -> String {
    let extended = info.extended(version);
    let fields: String = [
        extended
            .external_power
            .map(|value| format!(" external_power={:#04x}", value.raw())),
        extended
            .fast_charging
            .map(|value| format!(" fast_charging={:#04x}", value.raw())),
        extended
            .removable_battery
            .map(|value| format!(" removable_battery={:#04x}", value.raw())),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!(
        "0x1004 UnifiedBattery: version={version} percentage={} level={:?} status={:?}{fields}",
        info.charging_percentage, info.level, info.status
    )
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
    /// Firmware number, decoded by the protocol layer from packed BCD, or from
    /// binary when the firmware sends binary.
    pub number: u8,
    /// Firmware revision, decoded by the protocol layer from packed BCD, or from
    /// binary when the firmware sends binary.
    pub revision: u8,
    /// Firmware build, decoded by the protocol layer from packed BCD, or from
    /// binary when the firmware sends binary.
    pub build: u16,
    /// Whether this is the entity currently running.
    pub active: bool,
    /// Product ID of the device on the link the request used, as
    /// [`DeviceEntityFirmwareInfo::transport_pid`] decides it. `None` for every
    /// entity whose transport-specific bytes are not known to hold a PID.
    pub transport_pid: Option<u16>,
    /// Reply bytes 9 to 15, the transport-specific region, verbatim. Their
    /// meaning depends on the entity and the link, and a device that fills
    /// them in is exactly the device a report is being collected for.
    pub transport_specific_info: [u8; 7],
}

impl From<DeviceEntityFirmwareInfo> for FirmwareEntityInfo {
    fn from(info: DeviceEntityFirmwareInfo) -> Self {
        Self {
            transport_pid: info.transport_pid(),
            kind: info.entity_type,
            prefix: info.firmware_prefix,
            number: info.firmware_number,
            revision: info.revision,
            build: info.build,
            active: info.active,
            transport_specific_info: info.transport_specific_info,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
            // decode (a firmware prefix that is not UTF-8). The rest of the
            // table is still worth reading.
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
