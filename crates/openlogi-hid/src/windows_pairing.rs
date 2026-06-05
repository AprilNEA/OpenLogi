//! Windows Bluetooth pairing through the OS device-pairing API.
//!
//! Receiver pairing is still handled by [`crate::pairing`]. This module is the
//! Windows fallback for Bluetooth-direct devices: enumerate unpaired Bluetooth
//! endpoints and ask Windows to run the pairing ceremony for the selected one.

use std::{fmt, time::Duration};

use thiserror::Error;

const ENUMERATION_TIMEOUT: Duration = Duration::from_secs(30);
// Only read by the Windows `imp` module's device watcher.
#[cfg(target_os = "windows")]
const WATCHER_ENUMERATION_TIMEOUT: Duration = Duration::from_secs(12);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsPairingDevice {
    pub id: String,
    pub name: String,
    pub is_paired: bool,
    pub can_pair: bool,
    pub likely_logitech: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsPairingOutcome {
    pub device: WindowsPairingDevice,
    pub status: WindowsPairingStatus,
}

impl WindowsPairingOutcome {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(
            self.status,
            WindowsPairingStatus::Paired | WindowsPairingStatus::AlreadyPaired
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsPairingStatus {
    Paired,
    NotReadyToPair,
    NotPaired,
    AlreadyPaired,
    ConnectionRejected,
    TooManyConnections,
    HardwareFailure,
    AuthenticationTimeout,
    AuthenticationNotAllowed,
    AuthenticationFailure,
    NoSupportedProfiles,
    ProtectionLevelCouldNotBeMet,
    AccessDenied,
    InvalidCeremonyData,
    PairingCanceled,
    OperationAlreadyInProgress,
    RequiredHandlerNotRegistered,
    RejectedByHandler,
    RemoteDeviceHasAssociation,
    Failed,
    Other(i32),
}

impl fmt::Display for WindowsPairingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Paired => "paired",
            Self::NotReadyToPair => "not ready to pair",
            Self::NotPaired => "not paired",
            Self::AlreadyPaired => "already paired",
            Self::ConnectionRejected => "connection rejected",
            Self::TooManyConnections => "too many connections",
            Self::HardwareFailure => "hardware failure",
            Self::AuthenticationTimeout => "authentication timed out",
            Self::AuthenticationNotAllowed => "authentication not allowed",
            Self::AuthenticationFailure => "authentication failed",
            Self::NoSupportedProfiles => "no supported profiles",
            Self::ProtectionLevelCouldNotBeMet => "protection level could not be met",
            Self::AccessDenied => "access denied",
            Self::InvalidCeremonyData => "invalid ceremony data",
            Self::PairingCanceled => "pairing canceled",
            Self::OperationAlreadyInProgress => "operation already in progress",
            Self::RequiredHandlerNotRegistered => "required handler not registered",
            Self::RejectedByHandler => "rejected by handler",
            Self::RemoteDeviceHasAssociation => "remote device already has an association",
            Self::Failed => "failed",
            Self::Other(code) => return write!(f, "unknown status {code}"),
        };
        f.write_str(text)
    }
}

#[derive(Clone, Debug, Error)]
pub enum WindowsPairingError {
    #[error("Windows Bluetooth pairing is only available on Windows")]
    Unsupported,
    #[error("No Windows Bluetooth pairing candidates were found.")]
    NoCandidates,
    #[error("Windows pairing timed out")]
    Timeout,
    #[error("Windows device not found: {0}")]
    NotFound(String),
    #[error("Windows device cannot pair: {0}")]
    NotPairable(String),
    #[error("Windows API error: {0}")]
    Api(String),
}

pub async fn list_windows_pairing_devices() -> Result<Vec<WindowsPairingDevice>, WindowsPairingError>
{
    timeout(ENUMERATION_TIMEOUT, async {
        tokio::task::spawn_blocking(imp::list_windows_pairing_devices)
            .await
            .map_err(|e| join_error(&e))?
    })
    .await
}

pub async fn pair_windows_device(
    device_id: String,
) -> Result<WindowsPairingOutcome, WindowsPairingError> {
    timeout(PAIRING_TIMEOUT, async move {
        tokio::task::spawn_blocking(move || imp::pair_windows_device(&device_id))
            .await
            .map_err(|e| join_error(&e))?
    })
    .await
}

async fn timeout<T, F>(duration: Duration, future: F) -> Result<T, WindowsPairingError>
where
    F: std::future::Future<Output = Result<T, WindowsPairingError>>,
{
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| WindowsPairingError::Timeout)?
}

fn join_error(error: &tokio::task::JoinError) -> WindowsPairingError {
    WindowsPairingError::Api(error.to_string())
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{
        WindowsPairingDevice, WindowsPairingError, WindowsPairingOutcome, WindowsPairingStatus,
    };
    use windows::{
        Devices::{
            Bluetooth::BluetoothLEDevice,
            Enumeration::{DeviceInformation, DevicePairingResultStatus, DeviceWatcher},
        },
        Foundation::TypedEventHandler,
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
        core::{HSTRING, IInspectable},
    };

    enum WatchEvent {
        Added(DeviceInformation),
        Completed,
    }

    pub fn list_windows_pairing_devices() -> Result<Vec<WindowsPairingDevice>, WindowsPairingError>
    {
        let _winrt = init_winrt()?;
        let selector = BluetoothLEDevice::GetDeviceSelectorFromPairingState(false)
            .map_err(|e| api_error(&e))?;
        let devices = find_unpaired_bluetooth_endpoints(&selector)?;
        let mut out = Vec::new();
        for device in devices {
            let candidate = pairing_device_from_info(&device)?;
            if !candidate.name.trim().is_empty() {
                out.push(candidate);
            }
        }
        out.sort_by(|a, b| {
            b.likely_logitech
                .cmp(&a.likely_logitech)
                .then_with(|| b.can_pair.cmp(&a.can_pair))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }

    pub fn pair_windows_device(
        device_id: &str,
    ) -> Result<WindowsPairingOutcome, WindowsPairingError> {
        let _winrt = init_winrt()?;
        let device = find_device_by_id(device_id)?;
        let candidate = pairing_device_from_info(&device)?;
        let pairing = device.Pairing().map_err(|e| api_error(&e))?;
        if pairing.IsPaired().map_err(|e| api_error(&e))? {
            return Ok(WindowsPairingOutcome {
                device: candidate,
                status: WindowsPairingStatus::AlreadyPaired,
            });
        }
        if !pairing.CanPair().map_err(|e| api_error(&e))? {
            return Err(WindowsPairingError::NotPairable(candidate.name));
        }

        let result = pairing
            .PairAsync()
            .map_err(|e| api_error(&e))?
            .get()
            .map_err(|e| api_error(&e))?;
        Ok(WindowsPairingOutcome {
            device: candidate,
            status: WindowsPairingStatus::from(result.Status().map_err(|e| api_error(&e))?),
        })
    }

    fn find_unpaired_bluetooth_endpoints(
        selector: &HSTRING,
    ) -> Result<Vec<DeviceInformation>, WindowsPairingError> {
        let watcher =
            DeviceInformation::CreateWatcherAqsFilter(selector).map_err(|e| api_error(&e))?;
        let (tx, rx) = std::sync::mpsc::channel();
        let added_tx = tx.clone();
        let added = TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, device| {
            if let Some(device) = device.cloned() {
                let _ = added_tx.send(WatchEvent::Added(device));
            }
            Ok(())
        });
        let completed = TypedEventHandler::<DeviceWatcher, IInspectable>::new(move |_, _| {
            let _ = tx.send(WatchEvent::Completed);
            Ok(())
        });
        let added_token = watcher.Added(&added).map_err(|e| api_error(&e))?;
        let completed_token = watcher
            .EnumerationCompleted(&completed)
            .map_err(|e| api_error(&e))?;
        watcher.Start().map_err(|e| api_error(&e))?;

        let mut out = Vec::new();
        loop {
            match rx.recv_timeout(super::WATCHER_ENUMERATION_TIMEOUT) {
                Ok(WatchEvent::Added(device)) => out.push(device),
                Ok(WatchEvent::Completed) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        stop_watcher(&watcher, added_token, completed_token);
        Ok(out)
    }

    fn find_device_by_id(id: &str) -> Result<DeviceInformation, WindowsPairingError> {
        let selector = BluetoothLEDevice::GetDeviceSelectorFromPairingState(false)
            .map_err(|e| api_error(&e))?;
        find_unpaired_bluetooth_endpoints(&selector)?
            .into_iter()
            .find(|device| {
                device
                    .Id()
                    .is_ok_and(|actual| actual.to_string_lossy() == id)
            })
            .ok_or_else(|| WindowsPairingError::NotFound(id.to_string()))
    }

    fn pairing_device_from_info(
        device: &DeviceInformation,
    ) -> Result<WindowsPairingDevice, WindowsPairingError> {
        let name = device.Name().map_err(|e| api_error(&e))?.to_string_lossy();
        let pairing = device.Pairing().map_err(|e| api_error(&e))?;
        Ok(WindowsPairingDevice {
            id: device.Id().map_err(|e| api_error(&e))?.to_string_lossy(),
            likely_logitech: is_likely_logitech(&name),
            name,
            is_paired: pairing.IsPaired().map_err(|e| api_error(&e))?,
            can_pair: pairing.CanPair().map_err(|e| api_error(&e))?,
        })
    }

    fn is_likely_logitech(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        [
            "logitech",
            "logi",
            "mx ",
            "mx-",
            "ergo",
            "lift",
            "signature",
            "pebble",
            "k380",
            "k580",
            "k780",
            "m720",
            "m750",
            "m650",
            "m575",
        ]
        .iter()
        .any(|needle| name.contains(needle))
    }

    struct WinrtApartment;

    impl Drop for WinrtApartment {
        #[expect(unsafe_code, reason = "WinRT apartment cleanup requires FFI")]
        fn drop(&mut self) {
            unsafe {
                RoUninitialize();
            }
        }
    }

    #[expect(unsafe_code, reason = "WinRT apartment initialization requires FFI")]
    fn init_winrt() -> Result<WinrtApartment, WindowsPairingError> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).map_err(|e| api_error(&e))?;
        }
        Ok(WinrtApartment)
    }

    fn api_error(error: &windows::core::Error) -> WindowsPairingError {
        WindowsPairingError::Api(error.to_string())
    }

    fn stop_watcher(watcher: &DeviceWatcher, added_token: i64, completed_token: i64) {
        let _ = watcher.Stop();
        let _ = watcher.RemoveAdded(added_token);
        let _ = watcher.RemoveEnumerationCompleted(completed_token);
    }

    impl From<DevicePairingResultStatus> for WindowsPairingStatus {
        fn from(status: DevicePairingResultStatus) -> Self {
            match status {
                DevicePairingResultStatus::Paired => Self::Paired,
                DevicePairingResultStatus::NotReadyToPair => Self::NotReadyToPair,
                DevicePairingResultStatus::NotPaired => Self::NotPaired,
                DevicePairingResultStatus::AlreadyPaired => Self::AlreadyPaired,
                DevicePairingResultStatus::ConnectionRejected => Self::ConnectionRejected,
                DevicePairingResultStatus::TooManyConnections => Self::TooManyConnections,
                DevicePairingResultStatus::HardwareFailure => Self::HardwareFailure,
                DevicePairingResultStatus::AuthenticationTimeout => Self::AuthenticationTimeout,
                DevicePairingResultStatus::AuthenticationNotAllowed => {
                    Self::AuthenticationNotAllowed
                }
                DevicePairingResultStatus::AuthenticationFailure => Self::AuthenticationFailure,
                DevicePairingResultStatus::NoSupportedProfiles => Self::NoSupportedProfiles,
                DevicePairingResultStatus::ProtectionLevelCouldNotBeMet => {
                    Self::ProtectionLevelCouldNotBeMet
                }
                DevicePairingResultStatus::AccessDenied => Self::AccessDenied,
                DevicePairingResultStatus::InvalidCeremonyData => Self::InvalidCeremonyData,
                DevicePairingResultStatus::PairingCanceled => Self::PairingCanceled,
                DevicePairingResultStatus::OperationAlreadyInProgress => {
                    Self::OperationAlreadyInProgress
                }
                DevicePairingResultStatus::RequiredHandlerNotRegistered => {
                    Self::RequiredHandlerNotRegistered
                }
                DevicePairingResultStatus::RejectedByHandler => Self::RejectedByHandler,
                DevicePairingResultStatus::RemoteDeviceHasAssociation => {
                    Self::RemoteDeviceHasAssociation
                }
                DevicePairingResultStatus::Failed => Self::Failed,
                other => Self::Other(other.0),
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{WindowsPairingDevice, WindowsPairingError, WindowsPairingOutcome};

    pub fn list_windows_pairing_devices() -> Result<Vec<WindowsPairingDevice>, WindowsPairingError>
    {
        Err(WindowsPairingError::Unsupported)
    }

    pub fn pair_windows_device(
        _device_id: &str,
    ) -> Result<WindowsPairingOutcome, WindowsPairingError> {
        Err(WindowsPairingError::Unsupported)
    }
}
