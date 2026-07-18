//! OS HID hotplug events, bridged from the shared `async-hid` backend.

use futures_lite::{Stream, StreamExt as _};

use crate::inventory::InventoryError;
use crate::transport::hid_backend;

/// A HID node appeared on or vanished from the OS device tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugEvent {
    /// A device node was connected.
    Connected,
    /// A device node was disconnected.
    Disconnected,
}

/// Subscribe to OS HID hotplug events through the shared process-wide backend.
pub fn watch_hotplug() -> Result<impl Stream<Item = HotplugEvent> + Send + Unpin, InventoryError> {
    let stream = hid_backend().watch().map_err(InventoryError::Hid)?;
    Ok(stream.map(|event| match event {
        async_hid::DeviceEvent::Connected(_) => HotplugEvent::Connected,
        async_hid::DeviceEvent::Disconnected(_) => HotplugEvent::Disconnected,
    }))
}
