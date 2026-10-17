//! HID++ transport and channel lifecycle.
//!
//! Resolving a [`route::DeviceRoute`] to an open channel, and the strategies
//! that keep one open: [`ChannelPool`] for sessions that open on demand,
//! [`ChannelRegistry`] for channels owned by the inventory enumerator, and
//! [`SharedChannel`] handles lent out to this crate's read/write entry points.
//!
//! Opening itself belongs to a [`crate::backend::HidBackend`]; nothing here
//! names a HID stack.

use std::sync::{Arc, Weak};

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

    /// The physical route this channel addresses, used to serialize complete
    /// native lighting transactions rather than just individual HID reports.
    #[must_use]
    pub fn route(&self) -> &DeviceRoute {
        &self.route
    }

    /// A handle to this same connection that does not keep it open, for
    /// state that must not outlive the connection it was set through.
    #[must_use]
    pub fn downgrade(&self) -> WeakSharedChannel {
        WeakSharedChannel {
            channel: Arc::downgrade(&self.channel),
            route: self.route.clone(),
        }
    }
}

/// A [`SharedChannel`] that does not keep its connection open. Two handles are
/// equal when they name the same connection to the same route, so a replug or
/// another device answering on the route compares unequal.
#[derive(Clone)]
pub struct WeakSharedChannel {
    channel: Weak<HidppChannel>,
    route: DeviceRoute,
}

impl WeakSharedChannel {
    /// The channel, while its connection is still open.
    #[must_use]
    pub fn upgrade(&self) -> Option<SharedChannel> {
        self.channel
            .upgrade()
            .map(|channel| SharedChannel::new(channel, self.route.clone()))
    }
}

impl PartialEq for WeakSharedChannel {
    fn eq(&self, other: &Self) -> bool {
        self.route == other.route && Weak::ptr_eq(&self.channel, &other.channel)
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::{ScriptedRawHidChannel, scripted_channel};
    use super::*;

    fn route(slot: u8) -> DeviceRoute {
        DeviceRoute::Unifying {
            receiver_uid: "weak-channel".into(),
            slot,
        }
    }

    async fn connection() -> Arc<HidppChannel> {
        let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(|_| None);
        scripted_channel(raw).await
    }

    #[tokio::test]
    async fn a_weak_channel_names_one_connection_without_keeping_it_open() {
        let receiver = connection().await;
        let shared = SharedChannel::new(Arc::clone(&receiver), route(1));
        let weak = shared.downgrade();
        assert!(weak == shared.downgrade());
        assert!(
            weak.upgrade()
                .is_some_and(|channel| channel.matches(&route(1)))
        );

        let reconnected = SharedChannel::new(connection().await, route(1));
        assert!(
            weak != reconnected.downgrade(),
            "a reconnect is another connection"
        );
        let other_slot = SharedChannel::new(Arc::clone(&receiver), route(2));
        assert!(
            weak != other_slot.downgrade(),
            "the same receiver, another slot"
        );

        drop((receiver, shared, other_slot));
        assert!(
            weak.upgrade().is_none(),
            "a closed connection is not reopened"
        );
    }
}
