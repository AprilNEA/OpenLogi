//! OS HID hotplug events.

pub use crate::backend::{HotplugEvent, HotplugStream};

use super::InventoryError;
use crate::backend::HidBackend;

/// Subscribe to OS HID hotplug events through the shared process-wide backend.
pub fn watch_hotplug(backend: &dyn HidBackend) -> Result<HotplugStream, InventoryError> {
    Ok(backend.watch()?)
}
