//! Virtual HID gamepad backends for OpenLogi's opt-in auxiliary controller mode.
//!
//! The agent owns create/destroy and feeds [`GamepadState`] snapshots; host
//! rumble is polled back for devices with haptic feedback. Platform backends
//! live behind [`create`].

#![deny(missing_docs)]

mod descriptor;
mod error;
mod state;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub use error::GamepadError;
pub use state::{GamepadState, Rumble};

/// Long-lived OS-visible virtual gamepad.
pub trait VirtualGamepad: Send {
    /// Replace the full pad state and emit an input report.
    ///
    /// # Errors
    ///
    /// Returns when the underlying virtual device rejects the report.
    fn set_state(&mut self, state: &GamepadState) -> Result<(), GamepadError>;

    /// Drain the latest host rumble request, if any.
    fn poll_rumble(&mut self) -> Option<Rumble>;

    /// Tear down the virtual device.
    ///
    /// # Errors
    ///
    /// Returns when the platform backend fails to destroy the device cleanly.
    fn shutdown(self: Box<Self>) -> Result<(), GamepadError>;
}

/// Create a platform virtual gamepad named `product_name`.
///
/// # Errors
///
/// - macOS: [`GamepadError::EntitlementRequired`] when Virtual HID is denied
/// - Windows: [`GamepadError::DriverMissing`] when ViGEmBus is absent
/// - Linux / any: I/O failures creating the node
pub fn create(product_name: &str) -> Result<Box<dyn VirtualGamepad>, GamepadError> {
    cfg_select! {
        target_os = "macos" => { macos::create(product_name) }
        target_os = "linux" => { linux::create(product_name) }
        target_os = "windows" => { windows::create(product_name) }
        _ => { Err(GamepadError::UnsupportedPlatform) }
    }
}
