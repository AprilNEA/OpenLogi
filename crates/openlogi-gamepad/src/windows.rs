//! Windows virtual gamepad backend.
//!
//! ViGEmBus client wiring is intentionally deferred: without the bus this
//! returns [`GamepadError::DriverMissing`] so the agent fails soft and the
//! rest of the workspace still compiles on Windows CI.

use crate::{GamepadError, VirtualGamepad};

/// Attempt to attach a ViGEm Xbox 360 target.
pub fn create(_product_name: &str) -> Result<Box<dyn VirtualGamepad>, GamepadError> {
    Err(GamepadError::DriverMissing)
}
