//! Official application firmware inspection and dual-bank updates.
//!
//! The agent supplies its authoritative channel and serializes the complete
//! transaction. This module never opens a competing connection or downloads
//! data. Package authenticity must be checked against the pinned release before
//! [`FirmwarePackage::parse`]. No command writes the active application bank.
//!
//! See `docs/FIRMWARE_UPDATES.md` at the repository root for the transaction,
//! authentication boundary, hardware evidence and supported flash layout.

use std::{sync::Arc, time::Duration};

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        device_information::{
            DeviceEntityFirmwareInfo, DeviceEntityType, DeviceInformationFeature,
        },
        dfu_resumable::{DfuCapabilities, DfuResumableFeature, DfuStatus},
        unified_battery::{BatteryLevel, BatteryStatus, UnifiedBatteryFeature},
    },
    protocol::v20::Hidpp20Error,
};
use openlogi_core::hid::{
    FirmwareBatteryCharge, FirmwareOffer, FirmwareStatus, FirmwareUpdateFailure,
    FirmwareUpdatePhase, FirmwareVersion,
};
use openlogi_device_registry::firmware::{FirmwareRelease, latest_release};

use crate::{
    DeviceRoute, SharedChannel,
    backend::HidBackend,
    write::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route},
};

mod package;
mod transfer;

use package::FirmwareImage;
pub use package::{FirmwarePackage, PackageError};
use transfer::{Transfer, TransferAction};

const UPDATE_DEADLINE: Duration = Duration::from_mins(20);
const MAX_RECOVERIES: usize = 20;

struct Inspection {
    unit_id: [u8; 4],
    active: u8,
    running: DeviceEntityFirmwareInfo,
    applications: Vec<(u8, DeviceEntityFirmwareInfo)>,
}

impl Inspection {
    fn running(&self) -> &DeviceEntityFirmwareInfo {
        &self.running
    }

    fn installable_release(&self) -> Option<&'static FirmwareRelease> {
        let running = self.running();
        latest_release(
            &running.firmware_prefix,
            running.firmware_number,
            running.transport_pid()?,
        )
        .filter(|release| release_version(release) >= firmware_version(running))
    }
}

fn firmware_version(info: &DeviceEntityFirmwareInfo) -> FirmwareVersion {
    FirmwareVersion {
        number: info.firmware_number,
        revision: info.revision,
        build: info.build,
    }
}

fn release_version(release: &FirmwareRelease) -> FirmwareVersion {
    FirmwareVersion {
        number: release.number,
        revision: release.revision,
        build: release.build,
    }
}

fn read_error(error: Hidpp20Error, feature: u16) -> WriteError {
    classify_hidpp_error(error, HidppOperation::ReadFirmware, feature)
}

fn update_error(error: Hidpp20Error) -> FirmwareUpdateFailure {
    FirmwareUpdateFailure::Device(classify_hidpp_error(
        error,
        HidppOperation::WriteFirmware,
        DfuResumableFeature::ID,
    ))
}

async fn device_on(shared: &SharedChannel) -> Result<Device, WriteError> {
    Device::new(Arc::clone(shared.channel()), shared.device_index())
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: shared.device_index(),
        })
}

async fn inspect(device: &mut Device) -> Result<Inspection, WriteError> {
    let feature = open_feature::<DeviceInformationFeature>(device).await?;
    let info = feature
        .get_device_info()
        .await
        .map_err(|error| read_error(error, DeviceInformationFeature::ID))?;
    if info.entity_count == 0 || info.entity_count > 16 {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadFirmware,
            feature_hex: DeviceInformationFeature::ID,
        });
    }
    let mut applications = Vec::new();
    for index in 0..info.entity_count {
        let firmware = feature
            .get_fw_info(index)
            .await
            .map_err(|error| read_error(error, DeviceInformationFeature::ID))?;
        if firmware.entity_type == DeviceEntityType::MainApplication {
            applications.push((index, firmware));
        }
    }
    let mut active = applications
        .iter()
        .filter(|(_, application)| application.active);
    let Some((index, running)) = active.next() else {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadFirmware,
            feature_hex: DeviceInformationFeature::ID,
        });
    };
    let index = *index;
    let running = running.clone();
    if active.next().is_some() {
        return Err(WriteError::UnsupportedResponse {
            operation: HidppOperation::ReadFirmware,
            feature_hex: DeviceInformationFeature::ID,
        });
    }
    Ok(Inspection {
        unit_id: info.unit_id,
        active: index,
        running,
        applications,
    })
}

/// Reads the running application version and the update decision on an
/// existing inventory-owned connection.
pub async fn get_firmware_status_on(shared: &SharedChannel) -> Result<FirmwareStatus, WriteError> {
    let mut device = device_on(shared).await?;
    let inspection = inspect(&mut device).await?;
    let running = inspection.running();
    let offer = match inspection.installable_release() {
        None => FirmwareOffer::Unavailable,
        Some(release) => match open_feature::<DfuResumableFeature>(&mut device).await {
            Ok(feature) => {
                let capabilities = feature
                    .get_capabilities()
                    .await
                    .map_err(|error| read_error(error, DfuResumableFeature::ID))?;
                if !capabilities.contains(DfuCapabilities::RESUMABLE | DfuCapabilities::FORCIBLE) {
                    FirmwareOffer::Unavailable
                } else if release_version(release) == firmware_version(running) {
                    FirmwareOffer::UpToDate
                } else {
                    FirmwareOffer::Available(release_version(release))
                }
            }
            Err(WriteError::FeatureUnsupported { .. }) => FirmwareOffer::Unavailable,
            Err(error) => return Err(error),
        },
    };
    Ok(FirmwareStatus {
        running: firmware_version(running),
        offer,
    })
}

/// Route-addressed firmware inspection for diagnostic callers. Long-running
/// agent clients should use [`get_firmware_status_on`] and their existing channel.
pub async fn get_firmware_status(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<FirmwareStatus, WriteError> {
    let route = route.clone();
    with_route(backend, &route.clone(), move |channel| async move {
        get_firmware_status_on(&SharedChannel::new(channel, route)).await
    })
    .await
}

/// Returns the pinned current or newer release for this live device. The caller downloads
/// exactly this descriptor; it never reconstructs identity/version matching.
pub async fn installable_release_on(
    shared: &SharedChannel,
) -> Result<Option<&'static FirmwareRelease>, WriteError> {
    let mut device = device_on(shared).await?;
    Ok(inspect(&mut device).await?.installable_release())
}

/// A preflight-checked inactive-bank update. It owns the selected image and
/// feature listeners; consuming it is the only path to activation.
pub struct PreparedFirmwareUpdate {
    feature: Arc<DfuResumableFeature>,
    image: FirmwareImage,
    pending: PendingFirmwareActivation,
}

/// Identity needed to verify activation after the device restarts. It keeps no
/// old channel alive, so the agent can retire and reacquire that connection.
#[derive(Debug, Clone)]
pub struct PendingFirmwareActivation {
    route: DeviceRoute,
    unit_id: [u8; 4],
    entity: u8,
    product_id: u16,
    version: FirmwareVersion,
}

/// Checks the live identity, both bank records, battery, RDFU capabilities and
/// idle update state before returning a token. No DFU write occurs here.
pub async fn prepare_update_on(
    shared: &SharedChannel,
    release: &'static FirmwareRelease,
    package: &FirmwarePackage,
) -> Result<PreparedFirmwareUpdate, FirmwareUpdateFailure> {
    let mut device = device_on(shared)
        .await
        .map_err(FirmwareUpdateFailure::Device)?;
    let inspection = inspect(&mut device)
        .await
        .map_err(FirmwareUpdateFailure::Device)?;
    if inspection.installable_release() != Some(release)
        || !matches!(inspection.active, 1 | 2)
        || inspection.applications.len() != 2
    {
        return Err(FirmwareUpdateFailure::WrongDevice);
    }
    let entity = 3 - inspection.active;
    let running = inspection.running();
    let inactive = inspection
        .applications
        .iter()
        .find(|(index, _)| *index == entity)
        .map(|(_, info)| info)
        .ok_or(FirmwareUpdateFailure::WrongDevice)?;
    if inactive.active
        || inactive.firmware_prefix != running.firmware_prefix
        || inactive.firmware_number != running.firmware_number
    {
        return Err(FirmwareUpdateFailure::WrongDevice);
    }
    let image = package
        .images
        .iter()
        .find(|image| image.entity == entity)
        .ok_or(FirmwareUpdateFailure::ImageRejected)?;
    if image.version != release_version(release)
        || !release.fits(&image.prefix, image.version.number, image.product_id)
    {
        return Err(FirmwareUpdateFailure::ImageRejected);
    }
    check_battery(&mut device).await?;
    let feature = open_feature::<DfuResumableFeature>(&mut device)
        .await
        .map_err(|error| match error {
            WriteError::FeatureUnsupported { .. } => FirmwareUpdateFailure::Unsupported,
            other => FirmwareUpdateFailure::Device(other),
        })?;
    if !feature
        .get_capabilities()
        .await
        .map_err(update_error)?
        .contains(DfuCapabilities::RESUMABLE | DfuCapabilities::FORCIBLE)
    {
        return Err(FirmwareUpdateFailure::Unsupported);
    }
    let initial = feature.get_dfu_status(entity).await.map_err(update_error)?;
    if initial.entity != entity || initial.status != DfuStatus::NotStarted {
        return Err(FirmwareUpdateFailure::Protocol(
            "an unknown staged update is already present".into(),
        ));
    }
    Ok(PreparedFirmwareUpdate {
        feature,
        image: image.clone(),
        pending: PendingFirmwareActivation {
            route: shared.route().clone(),
            unit_id: inspection.unit_id,
            entity,
            product_id: image.product_id,
            version: image.version,
        },
    })
}

async fn check_battery(device: &mut Device) -> Result<(), FirmwareUpdateFailure> {
    let battery = open_feature::<UnifiedBatteryFeature>(device)
        .await
        .map_err(FirmwareUpdateFailure::Device)?;
    let capabilities = battery.get_battery_capabilities().await.map_err(|error| {
        FirmwareUpdateFailure::Device(read_error(error, UnifiedBatteryFeature::ID))
    })?;
    let battery = battery.get_battery_info().await.map_err(|error| {
        FirmwareUpdateFailure::Device(read_error(error, UnifiedBatteryFeature::ID))
    })?;
    if !capabilities.percentage {
        return Err(FirmwareUpdateFailure::Unsupported);
    }
    if battery.charging_percentage > 100
        || !matches!(
            battery.status,
            BatteryStatus::Discharging
                | BatteryStatus::Charging
                | BatteryStatus::ChargingNearlyFull
                | BatteryStatus::Full
                | BatteryStatus::ChargingSlow
        )
    {
        return Err(FirmwareUpdateFailure::Protocol(
            "battery subsystem is not ready for an update".into(),
        ));
    }
    if !FirmwareBatteryCharge::from_percentage(Some(battery.charging_percentage)).is_sufficient()
        || !matches!(battery.level, BatteryLevel::Good | BatteryLevel::Full)
    {
        return Err(FirmwareUpdateFailure::BatteryLow {
            percent: battery.charging_percentage,
        });
    }
    Ok(())
}

impl PreparedFirmwareUpdate {
    /// Transfers the validated image and activates it only after every packet
    /// was acknowledged. The caller must drive this future to completion and
    /// retain its I/O lease; cancellation is cooperative between native writes.
    /// `should_stop` prevents the next command, including activation.
    pub async fn install(
        self,
        mut progress: impl FnMut(FirmwareUpdatePhase),
        mut should_stop: impl FnMut() -> bool,
    ) -> Result<PendingFirmwareActivation, FirmwareUpdateFailure> {
        let started = tokio::time::Instant::now();
        let events = self.feature.listen();
        let mut transfer = Transfer::new(&self.image);
        let mut recoveries = 0;
        let mut waits = 0;
        check_stop(started, &mut should_stop)?;
        let mut report = self
            .feature
            .start_dfu(self.image.entity, self.image.magic)
            .await
            .map_err(update_error)?;
        let mut recovering = false;
        progress(FirmwareUpdatePhase::Transferring { percent: 0 });
        loop {
            check_stop(started, &mut should_stop)?;
            let action = transfer.accept(report, recovering)?;
            progress(FirmwareUpdatePhase::Transferring {
                percent: transfer.percent(),
            });
            match action {
                TransferAction::Send(packet) => {
                    recovering = false;
                    match self.feature.transfer_dfu_data(packet).await {
                        Ok(next) => report = next,
                        Err(error @ Hidpp20Error::Channel(_)) => {
                            if recoveries >= MAX_RECOVERIES {
                                return Err(update_error(error));
                            }
                            recoveries += 1;
                            check_stop(started, &mut should_stop)?;
                            while events.try_recv().is_ok() {}
                            report = self
                                .feature
                                .get_dfu_status(self.image.entity)
                                .await
                                .map_err(update_error)?;
                            recovering = true;
                        }
                        Err(error) => return Err(update_error(error)),
                    }
                }
                TransferAction::Wait(delay) => {
                    waits += 1;
                    if waits > 2000 {
                        return Err(FirmwareUpdateFailure::Protocol(
                            "excessive update waits".into(),
                        ));
                    }
                    report = if let Ok(Ok(next)) = tokio::time::timeout(delay, events.recv()).await
                    {
                        next
                    } else {
                        if recoveries >= MAX_RECOVERIES {
                            return Err(FirmwareUpdateFailure::Protocol(
                                "too many status recoveries".into(),
                            ));
                        }
                        recoveries += 1;
                        check_stop(started, &mut should_stop)?;
                        recovering = true;
                        self.feature
                            .get_dfu_status(self.image.entity)
                            .await
                            .map_err(update_error)?
                    };
                }
                TransferAction::Complete => break,
            }
        }
        check_stop(started, &mut should_stop)?;
        // Report restart before sending the write: the device can disappear as
        // soon as it accepts apply and never echoes a response.
        progress(FirmwareUpdatePhase::Restarting);
        self.feature
            .apply_dfu(self.image.entity)
            .await
            .map_err(update_error)?;
        Ok(self.pending)
    }
}

fn check_stop(
    started: tokio::time::Instant,
    should_stop: &mut impl FnMut() -> bool,
) -> Result<(), FirmwareUpdateFailure> {
    if should_stop() || started.elapsed() >= UPDATE_DEADLINE {
        Err(FirmwareUpdateFailure::Protocol(
            "update stopped before activation".into(),
        ))
    } else {
        Ok(())
    }
}

impl PendingFirmwareActivation {
    /// Verifies the exact device restarted into the selected bank and target
    /// version. A caller may retry while inventory reconnects, within its own
    /// bounded restart deadline.
    pub async fn verify_on(&self, shared: &SharedChannel) -> Result<(), FirmwareUpdateFailure> {
        if !shared.matches(&self.route) {
            return Err(FirmwareUpdateFailure::WrongDevice);
        }
        let mut device = device_on(shared)
            .await
            .map_err(FirmwareUpdateFailure::Device)?;
        let inspection = inspect(&mut device)
            .await
            .map_err(FirmwareUpdateFailure::Device)?;
        let running = inspection.running();
        if inspection.unit_id != self.unit_id || running.transport_pid() != Some(self.product_id) {
            return Err(FirmwareUpdateFailure::WrongDevice);
        }
        if inspection.active != self.entity || firmware_version(running) != self.version {
            return Err(FirmwareUpdateFailure::NotActivated);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
