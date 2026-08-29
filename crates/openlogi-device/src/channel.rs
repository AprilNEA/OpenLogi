//! HID++ transport and channel lifecycle.
//!
//! Resolving a [`route::DeviceRoute`] to an open channel, and the strategies
//! that keep one open: [`ChannelPool`] for sessions that open on demand,
//! [`ChannelRegistry`] for channels owned by the inventory enumerator, and
//! [`SharedChannel`] handles lent out to this crate's read/write entry points.
//!
//! Opening itself belongs to a [`crate::backend::HidBackend`]; nothing here
//! names a HID stack.

use std::sync::Arc;

use hidpp::channel::HidppChannel;

use route::DeviceRoute;

pub(crate) mod pool;
pub(crate) mod registry;
pub(crate) mod route;
#[cfg(test)]
pub(crate) mod scripted;

pub use pool::ChannelPool;
pub use registry::ChannelRegistry;

/// An open HID++ channel to a device, shared so route-addressed reads and writes
/// can reuse an inventory- or capture-owned connection instead of
/// re-enumerating and opening a fresh channel each time (which costs ~100ms+).
///
/// Cheap to clone (an `Arc` plus the [`DeviceRoute`] it points at). Built by
/// the inventory registry or a standalone capture session.
#[derive(Clone)]
pub struct SharedChannel {
    channel: Arc<HidppChannel>,
    route: DeviceRoute,
}

impl SharedChannel {
    /// Wrap an open channel that reaches `route`.
    #[must_use]
    pub(crate) fn new(channel: Arc<HidppChannel>, route: DeviceRoute) -> Self {
        Self { channel, route }
    }

    /// Whether this channel reaches `route` — so the write path only reuses it
    /// for the device it actually points at.
    #[must_use]
    pub fn matches(&self, route: &DeviceRoute) -> bool {
        self.route == *route
    }

    pub(crate) fn channel(&self) -> &Arc<HidppChannel> {
        &self.channel
    }

    pub(crate) fn device_index(&self) -> u8 {
        self.route.device_index()
    }

    pub(crate) fn route(&self) -> &DeviceRoute {
        &self.route
    }
}
