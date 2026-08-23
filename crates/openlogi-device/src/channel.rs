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

/// Stable identity used to keep immutable metadata attached to one physical
/// device rather than merely to its receiver slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeviceCacheIdentity {
    Direct,
    Physical {
        unit_id: Option<[u8; 4]>,
        serial_number: Option<String>,
    },
}

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
    cache_identity: Option<DeviceCacheIdentity>,
}

impl SharedChannel {
    /// Wrap an open channel that reaches `route`.
    ///
    /// Standalone direct channels are device-specific and may cache immutable
    /// metadata. Receiver routes need inventory's physical identity, so they
    /// deliberately remain uncached when built outside the registry.
    #[must_use]
    pub(crate) fn new(channel: Arc<HidppChannel>, route: DeviceRoute) -> Self {
        let cache_identity =
            matches!(route, DeviceRoute::Direct { .. }).then_some(DeviceCacheIdentity::Direct);
        Self {
            channel,
            route,
            cache_identity,
        }
    }

    /// Wrap a registry-owned channel with the identity of the physical device
    /// currently occupying this route.
    #[must_use]
    pub(crate) fn with_cache_identity(
        channel: Arc<HidppChannel>,
        route: DeviceRoute,
        cache_identity: Option<DeviceCacheIdentity>,
    ) -> Self {
        Self {
            channel,
            route,
            cache_identity,
        }
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

    pub(crate) fn cache_identity(&self) -> Option<&DeviceCacheIdentity> {
        self.cache_identity.as_ref()
    }

    pub(crate) fn cache_identity_matches(&self, current: Option<&DeviceCacheIdentity>) -> bool {
        self.cache_identity() == current
    }
}
