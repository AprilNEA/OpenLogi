//! Confirmed manual DPI changes for the agent's native notification backend.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use openlogi_hid::{ChannelRegistry, DeviceRoute, Dpi, SharedChannel, WeakSharedChannel};
use tokio::sync::broadcast;

use crate::hardware::WriteTicket;

/// A sensor change that can become stale while waiting to be displayed.
#[derive(Clone)]
pub struct DpiNotice {
    /// Mouse whose sensor confirmed the change.
    pub route: DeviceRoute,
    /// The confirmed DPI.
    pub dpi: Dpi,
    ticket: WriteTicket,
    registry: ChannelRegistry,
    connection: WeakSharedChannel,
    created: Instant,
}

impl DpiNotice {
    /// A notification should never appear long after the change, or after
    /// a newer write has been requested for this mouse.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.created.elapsed() < Duration::from_secs(2)
            && self.ticket.is_current()
            && self
                .connection
                .upgrade()
                .is_some_and(|channel| self.registry.is_current(&channel))
    }
}

/// Publishes native DPI feedback independently of the existing overlay.
#[derive(Default)]
pub struct DpiNotifications {
    enabled: AtomicBool,
    changes: OnceLock<broadcast::Sender<DpiNotice>>,
}

impl DpiNotifications {
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Whether to collect and display notification feedback. With no native
    /// backend attached, the normal write needs no additional sensor read.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.active_sender().is_some()
    }

    fn active_sender(&self) -> Option<&broadcast::Sender<DpiNotice>> {
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        self.changes
            .get()
            .filter(|sender| sender.receiver_count() != 0)
    }

    /// Subscribe to new changes. A slow backend keeps only the newest events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<DpiNotice> {
        self.changes
            .get_or_init(|| broadcast::channel(16).0)
            .subscribe()
    }

    pub(crate) fn publish(
        &self,
        channel: &SharedChannel,
        registry: &ChannelRegistry,
        dpi: Dpi,
        ticket: &WriteTicket,
    ) {
        if let Some(sender) = self.active_sender()
            && ticket.is_current()
        {
            let _ = sender.send(DpiNotice {
                route: channel.route().clone(),
                dpi,
                ticket: ticket.clone(),
                registry: registry.clone(),
                connection: channel.downgrade(),
                created: Instant::now(),
            });
        }
    }
}
