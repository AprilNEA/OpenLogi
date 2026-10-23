//! Device firmware versions and updates — pure data, no I/O.
//!
//! The HID++ `0x0003` reads and the `0x00D1` update that produce these live in
//! `openlogi_device::firmware`; the agent downloads the image and publishes
//! the update's progress.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::error::WriteError;
use super::route::DeviceRoute;
use crate::device::BatteryLevel;

/// Whether a battery reading permits a firmware installation.
///
/// The GUI's confirmation and the device preflight both classify through
/// [`Self::from_reading`], so a device the dialog lets through is not refused
/// for its charge after the download. Battery status and live-device checks
/// remain part of device preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareBatteryCharge {
    /// A valid percentage meets the minimum and the device reports a
    /// comfortable level.
    Sufficient,
    /// A valid percentage is below the minimum, or the device itself reports
    /// a low or critical level.
    Low,
    /// No valid percentage or level is available yet.
    Unknown,
}

impl FirmwareBatteryCharge {
    /// Classifies a charge reading using the shared installation policy.
    #[must_use]
    pub fn from_reading(percentage: Option<u8>, level: BatteryLevel) -> Self {
        match (percentage, level) {
            (None, _) | (_, BatteryLevel::Unknown) => Self::Unknown,
            (Some(percent), _) if percent > 100 => Self::Unknown,
            (Some(percent), _) if percent < Self::minimum_percent() => Self::Low,
            (Some(_), BatteryLevel::Critical | BatteryLevel::Low) => Self::Low,
            (Some(_), BatteryLevel::Good | BatteryLevel::Full) => Self::Sufficient,
        }
    }

    /// The minimum percentage, for user-facing charging instructions.
    #[must_use]
    pub const fn minimum_percent() -> u8 {
        30
    }

    /// Whether charge alone permits installation; missing readings do not.
    #[must_use]
    pub fn is_sufficient(self) -> bool {
        self == Self::Sufficient
    }
}

/// An application firmware version as HID++ `0x0003` reports it:
/// `number.revision.build`, e.g. `27.3.19`.
///
/// Field order is the comparison order, so a derived [`Ord`] sorts releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FirmwareVersion {
    /// The firmware number: one per product line, constant across updates.
    pub number: u8,
    /// The revision.
    pub revision: u8,
    /// The build.
    pub build: u16,
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.number, self.revision, self.build)
    }
}

/// What a device runs and whether OpenLogi can update it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareStatus {
    /// The application firmware the device is running.
    pub running: FirmwareVersion,
    /// Whether an official release can be installed or reinstalled.
    pub offer: FirmwareOffer,
}

impl FirmwareStatus {
    /// The release an explicit installation request can select, including a
    /// reinstall of the exact current release when the device is up to date.
    #[must_use]
    pub fn installation_target(&self) -> Option<FirmwareVersion> {
        match self.offer {
            FirmwareOffer::Available(version) => Some(version),
            FirmwareOffer::UpToDate => Some(self.running),
            FirmwareOffer::Unavailable => None,
        }
    }
}

/// Whether a device can be updated, and to what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareOffer {
    /// OpenLogi knows no official firmware for this device, or the device
    /// cannot be updated over HID++.
    Unavailable,
    /// The device runs the newest release OpenLogi knows.
    UpToDate,
    /// A newer official release can be installed.
    Available(FirmwareVersion),
}

/// The firmware update the agent is running or last ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareUpdate {
    /// The device being updated.
    pub route: DeviceRoute,
    /// The release being installed.
    pub target: FirmwareVersion,
    /// Where the update stands.
    pub phase: FirmwareUpdatePhase,
    /// Monotonically increasing job identity within this agent run.
    pub id: u64,
    /// Bounded activity history recorded by the agent, including terminal errors.
    pub events: Vec<FirmwareUpdateEvent>,
    /// Approved package provenance retained with the job.
    pub source: Option<FirmwareUpdateSource>,
    /// The admission request that started this job.
    pub request: FirmwareRequest,
}

/// Names one firmware admission request.
///
/// The requester chooses it and the agent stores it in the job that request
/// starts, so a client recognizes its own job even when the admission reply
/// is lost. A route cannot do this: it names a connection slot, which another
/// physical device or another client's request can reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FirmwareRequest(pub u64);

impl FirmwareRequest {
    /// A fresh request identity, unpredictable across processes and distinct
    /// within one.
    #[must_use]
    pub fn generate() -> Self {
        use std::hash::{BuildHasher, RandomState};
        use std::sync::atomic::{AtomicU64, Ordering};
        static ISSUED: AtomicU64 = AtomicU64::new(0);
        // `RandomState` keys come from the OS once per thread and advance per
        // construction; the counter keeps two calls from hashing equal input.
        Self(RandomState::new().hash_one(ISSUED.fetch_add(1, Ordering::Relaxed)))
    }
}

/// Device family and package location selected by the agent's approved catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareUpdateSource {
    /// Marketed device family targeted by the package.
    pub device_name: String,
    /// Firmware family, such as `RBM27`.
    pub firmware_line: String,
    /// Official package URL requested by the downloader.
    pub package_url: String,
}

/// The version a catalog release installs. Every consumer converts through
/// this, so the release and the version a device reports compare one way.
impl From<&openlogi_device_registry::firmware::FirmwareRelease> for FirmwareVersion {
    fn from(release: &openlogi_device_registry::firmware::FirmwareRelease) -> Self {
        Self {
            number: release.number,
            revision: release.revision,
            build: release.build,
        }
    }
}

impl From<&openlogi_device_registry::firmware::FirmwareRelease> for FirmwareUpdateSource {
    fn from(release: &openlogi_device_registry::firmware::FirmwareRelease) -> Self {
        Self {
            device_name: release.device_name.into(),
            firmware_line: format!("{}{}", release.prefix, release.number),
            package_url: release.package_url.into(),
        }
    }
}

/// One observed firmware phase change, retained across client reconnects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareUpdateEvent {
    /// Agent wall-clock time in milliseconds since the Unix epoch.
    pub timestamp_millis: u64,
    /// The phase entered at this time. Failed phases are error entries.
    pub phase: FirmwareUpdatePhase,
}

/// Where a firmware update stands.
///
/// bincode encodes the variant *index*, so variants are append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareUpdatePhase {
    /// Fetching and verifying the official image.
    Downloading,
    /// Checking the device and opening the update.
    Preparing,
    /// Writing the image into the inactive bank.
    Transferring {
        /// How much of the image the device has acknowledged, `0..=100`.
        percent: u8,
    },
    /// The image was activated; waiting for the device to restart on it.
    Restarting,
    /// The device runs the new firmware.
    Succeeded,
    /// The update stopped; the reason describes the observed failure.
    Failed(FirmwareUpdateFailure),
}

impl FirmwareUpdatePhase {
    /// Whether the update has ended, successfully or not.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed(_))
    }
}

/// Why a firmware update stopped.
///
/// bincode encodes the variant *index*, so variants are append-only.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum FirmwareUpdateFailure {
    /// The image could not be downloaded.
    #[error("firmware download failed: {0}")]
    Download(String),
    /// The downloaded image is not the official release OpenLogi expects.
    #[error("the downloaded firmware did not match the official release")]
    ImageRejected,
    /// The image does not fit the device: another model, or a bank layout
    /// OpenLogi does not know.
    #[error("the firmware does not fit this device")]
    WrongDevice,
    /// The battery is too low to update safely.
    #[error("battery at {percent}% is too low to update the firmware")]
    BatteryLow {
        /// The charge the device reported.
        percent: u8,
    },
    /// The device lacks the update modes OpenLogi relies on.
    #[error("the device does not support firmware updates over HID++")]
    Unsupported,
    /// The device refused a step of the update.
    #[error("the device refused the update (status {code:#04x})")]
    DeviceRefused {
        /// The status code the device answered with.
        code: u8,
    },
    /// The device's answers broke the update protocol.
    #[error("the device answered out of order: {0}")]
    Protocol(String),
    /// Talking to the device failed, typically because it disconnected.
    #[error("device I/O failed: {0}")]
    Device(WriteError),
    /// The device restarted, but not into the new firmware.
    #[error("the device did not restart into the new firmware")]
    NotActivated,
}

/// Why the agent did not start a firmware update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum FirmwareUpdateRefusal {
    /// Another update is still running.
    #[error("another firmware update is running")]
    Busy,
    /// OpenLogi has no current or newer release it can install.
    #[error("no supported firmware is available for this device")]
    NoUpdate,
    /// The device is not connected.
    #[error("the device is not connected")]
    DeviceNotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_charge_requires_a_valid_percentage_of_at_least_thirty_and_a_good_level() {
        for level in [
            BatteryLevel::Critical,
            BatteryLevel::Low,
            BatteryLevel::Good,
            BatteryLevel::Full,
            BatteryLevel::Unknown,
        ] {
            assert_eq!(
                FirmwareBatteryCharge::from_reading(None, level),
                FirmwareBatteryCharge::Unknown
            );
            let comfortable = matches!(level, BatteryLevel::Good | BatteryLevel::Full);
            for percent in 0..=u8::MAX {
                let charge = FirmwareBatteryCharge::from_reading(Some(percent), level);
                assert_eq!(
                    charge.is_sufficient(),
                    comfortable && (30..=100).contains(&percent),
                    "{percent}% {level:?}"
                );
                let expected = if level == BatteryLevel::Unknown || percent > 100 {
                    FirmwareBatteryCharge::Unknown
                } else if percent < 30 || !comfortable {
                    FirmwareBatteryCharge::Low
                } else {
                    FirmwareBatteryCharge::Sufficient
                };
                assert_eq!(charge, expected, "{percent}% {level:?}");
            }
        }
    }

    #[test]
    fn installation_target_includes_explicit_reinstall_but_not_unavailable_firmware() {
        let running = FirmwareVersion {
            number: 27,
            revision: 3,
            build: 19,
        };
        let newer = FirmwareVersion {
            build: 20,
            ..running
        };
        for (offer, expected) in [
            (FirmwareOffer::Unavailable, None),
            (FirmwareOffer::UpToDate, Some(running)),
            (FirmwareOffer::Available(newer), Some(newer)),
        ] {
            assert_eq!(
                FirmwareStatus { running, offer }.installation_target(),
                expected
            );
        }
    }

    #[test]
    fn generated_requests_are_distinct() {
        let issued: std::collections::HashSet<_> =
            (0..1000).map(|_| FirmwareRequest::generate()).collect();
        assert_eq!(issued.len(), 1000);
    }
}
