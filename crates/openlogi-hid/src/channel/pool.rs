//! Shared HID++ channels for long-running agent sessions.

use std::sync::{Arc, Weak};

use hidpp::channel::HidppChannel;
use tokio::sync::Mutex;

use crate::backend::{BackendError, HidBackend};
use crate::channel::route::{DeviceRoute, open_route_channel};
use crate::channel::transport::native_backend;

/// Reuses one open HID++ channel for routes on the same receiver.
#[derive(Clone)]
pub struct ChannelPool {
    /// The HID stack routes are opened through. Defaults to the host's; tests
    /// and non-native hosts supply their own via [`ChannelPool::with_backend`].
    backend: Arc<dyn HidBackend>,
    entries: Arc<Mutex<Vec<PoolEntry>>>,
}

impl Default for ChannelPool {
    fn default() -> Self {
        Self::with_backend(native_backend())
    }
}

struct PoolEntry {
    route: DeviceRoute,
    channel: Weak<HidppChannel>,
}

impl ChannelPool {
    /// Build a pool that opens through `backend` instead of the host's HID
    /// stack.
    #[must_use]
    pub fn with_backend(backend: Arc<dyn HidBackend>) -> Self {
        Self {
            backend,
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Return a shared channel reaching `route`, opening it when necessary.
    pub async fn open(
        &self,
        route: &DeviceRoute,
    ) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        let mut entries = self.entries.lock().await;
        entries.retain(|entry| entry.channel.strong_count() > 0);
        if let Some(channel) = entries.iter().find_map(|entry| {
            entry
                .route
                .shares_transport(route)
                .then(|| entry.channel.upgrade())
                .flatten()
        }) {
            return Ok(Some(channel));
        }
        let Some(channel) = open_route_channel(&*self.backend, route).await? else {
            return Ok(None);
        };
        entries.push(PoolEntry {
            route: route.clone(),
            channel: Arc::downgrade(&channel),
        });
        Ok(Some(channel))
    }
}
