//! Failure modes for virtual gamepad create/emit/destroy.

use thiserror::Error;

/// Why a virtual gamepad operation failed.
#[derive(Debug, Error)]
pub enum GamepadError {
    /// macOS denied `IOHIDUserDeviceCreate` — needs
    /// `com.apple.developer.hid.virtual.device` on the agent.
    #[error(
        "macOS Virtual HID entitlement required \
         (com.apple.developer.hid.virtual.device on the agent)"
    )]
    EntitlementRequired,

    /// Windows ViGEmBus is not installed or the client could not attach.
    #[error("ViGEmBus driver missing or unavailable")]
    DriverMissing,

    /// This OS has no backend yet.
    #[error("virtual gamepad is not supported on this platform")]
    UnsupportedPlatform,

    /// Underlying I/O or FFI failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
