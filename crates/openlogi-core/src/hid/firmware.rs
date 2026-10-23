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

/// Whether a reported charge percentage meets the firmware installation minimum.
/// Other battery health and live-device checks remain part of device preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareBatteryCharge {
    /// A valid percentage meets the minimum.
    Sufficient,
    /// A valid percentage is below the minimum.
    Low,
    /// No valid percentage is available yet.
    Unknown,
}

impl FirmwareBatteryCharge {
    /// Classifies a charge reading using the shared installation policy.
    #[must_use]
    pub fn from_percentage(percentage: Option<u8>) -> Self {
        match percentage {
            Some(percent) if percent > 100 => Self::Unknown,
            Some(percent) if percent < Self::minimum_percent() => Self::Low,
            Some(_) => Self::Sufficient,
            None => Self::Unknown,
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
    fn firmware_charge_requires_a_valid_percentage_of_at_least_thirty() {
        assert_eq!(
            FirmwareBatteryCharge::from_percentage(None),
            FirmwareBatteryCharge::Unknown
        );
        for percent in 0..=u8::MAX {
            let charge = FirmwareBatteryCharge::from_percentage(Some(percent));
            assert_eq!(charge.is_sufficient(), (30..=100).contains(&percent));
            if percent < 30 {
                assert_eq!(charge, FirmwareBatteryCharge::Low);
            } else if percent > 100 {
                assert_eq!(charge, FirmwareBatteryCharge::Unknown);
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
}
