//! Shared HID++ channels for long-running agent sessions.

use std::sync::{Arc, Weak};

use hidpp::channel::HidppChannel;
use tokio::sync::Mutex;

use crate::backend::BackendError;
use crate::channel::route::{DeviceRoute, open_route_channel};

/// Reuses one open HID++ channel for routes on the same receiver.
#[derive(Clone, Default)]
pub struct ChannelPool {
    entries: Arc<Mutex<Vec<PoolEntry>>>,
}

struct PoolEntry {
    route: DeviceRoute,
    channel: Weak<HidppChannel>,
}

impl ChannelPool {
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
        let Some(channel) = open_route_channel(route).await? else {
            return Ok(None);
        };
        entries.push(PoolEntry {
            route: route.clone(),
            channel: Arc::downgrade(&channel),
        });
        Ok(Some(channel))
    }
}
