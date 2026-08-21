//! OS HID hotplug events.

use futures_lite::Stream;

pub use crate::backend::HotplugEvent;

use super::InventoryError;
use crate::channel::transport::native_backend;

/// Subscribe to OS HID hotplug events through the shared process-wide backend.
pub fn watch_hotplug() -> Result<impl Stream<Item = HotplugEvent> + Send + Unpin, InventoryError> {
    Ok(native_backend().watch()?)
}
