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
    device::{Device, DeviceError},
    feature::{
        CreatableFeature,
        device_information::{
            DeviceEntityFirmwareInfo, DeviceEntityType, DeviceInformationFeature,
        },
        dfu_resumable::{DfuCapabilities, DfuReply, DfuResumableFeature, DfuStatus},
        unified_battery::{BatteryStatus, UnifiedBatteryFeature},
    },
    protocol::v20::Hidpp20Error,
};
use openlogi_core::hid::{
    FirmwareBatteryCharge, FirmwareOffer, FirmwareStatus, FirmwareUpdateFailure,
    FirmwareUpdatePhase, FirmwareUpdateWarning, FirmwareVersion,
};
use openlogi_device_registry::firmware::{FirmwareRelease, latest_release};
use tracing::warn;

use crate::{
    DeviceRoute, SharedChannel,
    backend::HidBackend,
    inventory::mappings::map_battery_level,
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
        .filter(|release| FirmwareVersion::from(*release) >= firmware_version(running))
    }
}

fn firmware_version(info: &DeviceEntityFirmwareInfo) -> FirmwareVersion {
    FirmwareVersion {
        number: info.firmware_number,
        revision: info.revision,
        build: info.build,
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
        .map_err(|error| match error {
            // A HID++ 1.0 device has no `0x0003` firmware table to read; that
            // is permanent, not a device that failed to answer.
            DeviceError::UnsupportedProtocolVersion => WriteError::FeatureUnsupported {
                feature_hex: DeviceInformationFeature::ID,
            },
            _ => WriteError::DeviceUnreachable {
                index: shared.device_index(),
            },
        })
}

/// The device answered, but its firmware table has no single running
/// application: it does not describe firmware OpenLogi can show or update.
/// Permanent for the device, so it is reported as unsupported rather than as
/// a read that might succeed on retry.
fn no_running_application() -> WriteError {
    WriteError::FeatureUnsupported {
        feature_hex: DeviceInformationFeature::ID,
    }
}

async fn inspect(device: &mut Device) -> Result<Inspection, WriteError> {
    let feature = open_feature::<DeviceInformationFeature>(device).await?;
    let info = feature
        .get_device_info()
        .await
        .map_err(|error| read_error(error, DeviceInformationFeature::ID))?;
    if info.entity_count == 0 || info.entity_count > 16 {
        return Err(no_running_application());
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
        return Err(no_running_application());
    };
    let index = *index;
    let running = running.clone();
    if active.next().is_some() {
        return Err(no_running_application());
    }
    Ok(Inspection {
        unit_id: info.unit_id,
        active: index,
        running,
        applications,
    })
}

/// Whether an inactive application bank reports an all-zero record: no
/// firmware prefix, version, build, active flag or transport bytes.
///
/// An interrupted transfer leaves the inactive bank in this state. Observed,
/// not specified: an MX Master 4 (`RBM` 27.3.19, Bluetooth) whose reinstall
/// was stopped mid-transfer reported exactly this record for entity 1 while it
/// kept running entity 2, presumably because the device clears the bank when
/// a transfer begins. Without this exception the inactive-bank line check
/// refuses every retry, so OpenLogi could never refill a bank its own
/// interrupted run cleared. A blank record names no other firmware line, so it
/// cannot be the mismatch that check guards against; a bank that reports a
/// different line is still refused.
fn is_erased_bank(info: &DeviceEntityFirmwareInfo) -> bool {
    info.entity_type == DeviceEntityType::MainApplication
        && !info.active
        && info.firmware_prefix.bytes().all(|byte| byte == 0)
        && info.firmware_number == 0
        && info.revision == 0
        && info.build == 0
        && info.transport_specific_info == [0; 7]
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
                } else if FirmwareVersion::from(release) == firmware_version(running) {
                    FirmwareOffer::UpToDate
                } else {
                    FirmwareOffer::Available(FirmwareVersion::from(release))
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

/// A selected update bound to the physical device inspected at admission.
/// Private identity fields must survive download and I/O ownership waits.
#[derive(Debug, PartialEq, Eq)]
pub struct FirmwareUpdateTarget {
    route: DeviceRoute,
    unit_id: [u8; 4],
    release: &'static FirmwareRelease,
}

impl FirmwareUpdateTarget {
    /// The approved package to download and describe in progress updates.
    #[must_use]
    pub fn release(&self) -> &'static FirmwareRelease {
        self.release
    }
}

/// Selects a current or newer approved release and retains the live device's
/// identity. Preflight consumes this target rather than selecting a new unit.
pub async fn update_target_on(
    shared: &SharedChannel,
) -> Result<Option<FirmwareUpdateTarget>, WriteError> {
    let mut device = device_on(shared).await?;
    let inspection = inspect(&mut device).await?;
    Ok(inspection
        .installable_release()
        .map(|release| FirmwareUpdateTarget {
            route: shared.route().clone(),
            unit_id: inspection.unit_id,
            release,
        }))
}

/// A preflight-checked inactive-bank update. It owns the selected image and
/// the DFU feature; consuming it is the only path to activation.
pub struct PreparedFirmwareUpdate {
    feature: Arc<DfuResumableFeature>,
    /// Damage an earlier run left on the target bank, reported to the
    /// activity log before the transfer starts.
    warnings: Vec<FirmwareUpdateWarning>,
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
    target: FirmwareUpdateTarget,
    package: &FirmwarePackage,
) -> Result<PreparedFirmwareUpdate, FirmwareUpdateFailure> {
    if !shared.matches(&target.route) {
        return Err(FirmwareUpdateFailure::WrongDevice);
    }
    let mut device = device_on(shared)
        .await
        .map_err(FirmwareUpdateFailure::Device)?;
    let inspection = inspect(&mut device)
        .await
        .map_err(FirmwareUpdateFailure::Device)?;
    let release = target.release;
    if inspection.unit_id != target.unit_id
        || inspection.installable_release() != Some(release)
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
        || !(is_erased_bank(inactive)
            || (inactive.firmware_prefix == running.firmware_prefix
                && inactive.firmware_number == running.firmware_number))
    {
        return Err(FirmwareUpdateFailure::WrongDevice);
    }
    let image = package
        .images
        .iter()
        .find(|image| image.entity == entity)
        .ok_or(FirmwareUpdateFailure::ImageRejected)?;
    if image.version != FirmwareVersion::from(release)
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
    let initial = feature
        .get_dfu_status(entity)
        .await
        .map_err(update_error)?
        .report;
    if initial.entity != entity {
        return Err(FirmwareUpdateFailure::Protocol(
            "status names a different firmware bank".into(),
        ));
    }
    // An interrupted run (sleep, agent restart, timeout, protocol error) can
    // leave any transfer state behind. Nothing proves its bytes came from this
    // package, so it is never resumed or applied: install restarts the
    // transfer and must send every packet itself.
    let interrupted = initial.status != DfuStatus::NotStarted;
    let warnings = [
        is_erased_bank(inactive).then_some(FirmwareUpdateWarning::ErasedBank { entity }),
        interrupted.then_some(FirmwareUpdateWarning::UnfinishedTransfer { entity }),
    ]
    .into_iter()
    .flatten()
    .collect();
    Ok(PreparedFirmwareUpdate {
        feature,
        warnings,
        image: image.clone(),
        pending: PendingFirmwareActivation {
            route: target.route,
            unit_id: target.unit_id,
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
    if !FirmwareBatteryCharge::from_reading(
        Some(battery.charging_percentage),
        map_battery_level(battery.level),
    )
    .is_sufficient()
    {
        return Err(FirmwareUpdateFailure::BatteryLow {
            percent: battery.charging_percentage,
        });
    }
    Ok(())
}

impl PreparedFirmwareUpdate {
    /// Preflight found an earlier run's transfer state for the target bank.
    fn interrupted(&self) -> bool {
        self.warnings
            .iter()
            .any(|w| matches!(w, FirmwareUpdateWarning::UnfinishedTransfer { .. }))
    }

    /// Transfers the validated image and activates it only after every packet
    /// was acknowledged. The caller must drive this future to completion and
    /// retain its I/O lease; cancellation is cooperative between native writes.
    /// `should_stop` prevents the next command, including activation.
    ///
    /// Success means activation was requested, not that it happened: only
    /// [`PendingFirmwareActivation::verify_on`] establishes what the device
    /// runs afterwards.
    pub async fn install(
        self,
        mut progress: impl FnMut(FirmwareUpdatePhase),
        mut should_stop: impl FnMut() -> bool,
    ) -> Result<PendingFirmwareActivation, FirmwareUpdateFailure> {
        let started = tokio::time::Instant::now();
        // Logged before any DFU command, so the activity log shows what was
        // found and what this run does about it even if the start fails.
        for warning in &self.warnings {
            progress(FirmwareUpdatePhase::Warning(*warning));
        }
        let mut transfer = Transfer::new(&self.image);
        let mut recoveries = 0;
        let mut waits = 0;
        check_stop(started, &mut should_stop)?;
        let (mut reply, mut recovering) = match self
            .feature
            .start_dfu(self.image.entity, self.image.magic)
            .await
        {
            Ok(reply) => (reply, false),
            // After an interruption, a status query cannot tell this run's
            // restart apart from the state the earlier run left behind.
            Err(Hidpp20Error::Channel(_)) if self.interrupted() => {
                return Err(FirmwareUpdateFailure::Protocol(
                    "restarting an interrupted update was not confirmed".into(),
                ));
            }
            Err(Hidpp20Error::Channel(_)) => {
                // A start may have reached the device despite a lost reply.
                // Keep this admission and query; never blindly start again.
                recoveries += 1;
                check_stop(started, &mut should_stop)?;
                // Transfer's initial transition accepts only this entity at
                // block zero. NotStarted, waits and advanced states fail closed.
                (self.poll_status().await?, true)
            }
            Err(error) => return Err(update_error(error)),
        };
        let mut report = reply.report;
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
                    reply = match self.feature.transfer_dfu_data(packet).await {
                        Ok(next) => next,
                        Err(error @ Hidpp20Error::Channel(_)) => {
                            if recoveries >= MAX_RECOVERIES {
                                return Err(update_error(error));
                            }
                            recoveries += 1;
                            check_stop(started, &mut should_stop)?;
                            recovering = true;
                            self.poll_status().await?
                        }
                        Err(error) => return Err(update_error(error)),
                    };
                    report = reply.report;
                }
                TransferAction::Wait(delay) => {
                    waits += 1;
                    if waits > 2000 {
                        return Err(FirmwareUpdateFailure::Protocol(
                            "excessive update waits".into(),
                        ));
                    }
                    // Only a broadcast sent after the reply that asked for
                    // this wait can end it; an earlier one is a stale state.
                    let broadcast = tokio::time::timeout(delay, reply.next_broadcast()).await;
                    if let Ok(Some(next)) = broadcast {
                        report = next;
                    } else {
                        if recoveries >= MAX_RECOVERIES {
                            return Err(FirmwareUpdateFailure::Protocol(
                                "too many status recoveries".into(),
                            ));
                        }
                        recoveries += 1;
                        check_stop(started, &mut should_stop)?;
                        recovering = true;
                        reply = self.poll_status().await?;
                        report = reply.report;
                    }
                }
                TransferAction::Complete => break,
            }
        }
        check_stop(started, &mut should_stop)?;
        // Report restart before sending the write: the device can disappear as
        // soon as it accepts apply and never echoes a response.
        progress(FirmwareUpdatePhase::Restarting);
        if let Err(error) = self.feature.apply_dfu(self.image.entity).await {
            // A device that accepts apply reboots at once, and a link that
            // drops with it (typically Bluetooth) fails the write even though
            // the device may already run the new image. The outcome is
            // unknown, so the restart check decides it.
            warn!(%error, "firmware apply write failed; verifying the restart");
        }
        Ok(self.pending)
    }

    async fn poll_status(&self) -> Result<DfuReply, FirmwareUpdateFailure> {
        self.feature
            .get_dfu_status(self.image.entity)
            .await
            .map_err(update_error)
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
