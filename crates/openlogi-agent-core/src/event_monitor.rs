//! Live event monitor: a shared, bounded buffer that mirrors the events the OS
//! mouse hook observes to the GUI's debug monitor, on demand.
//!
//! Monitoring is **off by default**. The freeze-sensitive hook callback pays
//! only a single relaxed atomic load per event while off (see the freeze-hazard
//! note in `openlogi-hook`); it locks and pushes only once the GUI starts
//! polling. The GUI enables monitoring implicitly by polling
//! [`EventMonitor::poll`], and [`EventMonitor::run_idle_janitor`] turns it back
//! off when polls stop — so a closed panel or a crashed GUI can't leave the
//! callback doing buffer work forever.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openlogi_hook::MouseEvent;

use crate::ipc::MonitorEvent;

/// A shared [`EventMonitor`], threaded between the hook callback (writer) and
/// the IPC server (reader/poller).
pub type SharedEventMonitor = std::sync::Arc<EventMonitor>;

/// How many recent events to retain between polls. A held button + a flick of
/// the scroll wheel is a handful of events; a generous cap still drops only the
/// oldest if the GUI stalls.
const CAPACITY: usize = 256;

/// How often the janitor checks for an idle (no-longer-polled) monitor.
const IDLE_TICK: Duration = Duration::from_secs(3);

/// Buffers the hook's observed events for the GUI's live monitor when enabled.
#[derive(Default)]
pub struct EventMonitor {
    enabled: AtomicBool,
    /// Set on every [`Self::poll`]; the janitor clears it each tick and treats a
    /// tick with no intervening poll as "the GUI stopped watching".
    polled: AtomicBool,
    buf: Mutex<VecDeque<MonitorEvent>>,
}

impl EventMonitor {
    /// Whether monitoring is currently on — the one check the hot hook path runs.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Record a hook event, if monitoring is on. Pointer moves are dropped: they
    /// arrive at pointer-motion rates and would evict every button/scroll event
    /// from the bounded buffer before the GUI's next poll.
    pub fn record(&self, event: &MouseEvent) {
        if !self.enabled() {
            return;
        }
        let mapped = match event {
            MouseEvent::Button { id, pressed } => MonitorEvent::Button {
                button: id.to_string(),
                pressed: *pressed,
            },
            MouseEvent::Scroll {
                delta_x, delta_y, ..
            } => MonitorEvent::Scroll {
                delta_x: *delta_x,
                delta_y: *delta_y,
            },
            MouseEvent::CaptureInterrupted => MonitorEvent::CaptureInterrupted,
            MouseEvent::Moved { .. } => return,
        };
        if let Ok(mut buf) = self.buf.lock() {
            if buf.len() == CAPACITY {
                buf.pop_front();
            }
            buf.push_back(mapped);
        }
    }

    /// Enable monitoring (idempotent) and drain everything buffered since the
    /// last poll. Called from the IPC `poll_event_monitor` handler.
    pub fn poll(&self) -> Vec<MonitorEvent> {
        // Mark the poll *before* enabling, and publish `enabled` with `Release`.
        // The janitor loads `enabled` with `Acquire`, so a tick that observes
        // `enabled == true` is guaranteed to also see this `polled = true` and
        // won't disable a monitor that was just enabled by this very poll.
        self.polled.store(true, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
        self.buf
            .lock()
            .map(|mut buf| buf.drain(..).collect())
            .unwrap_or_default()
    }

    /// Turn monitoring off and discard any buffered events.
    fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
        if let Ok(mut buf) = self.buf.lock() {
            buf.clear();
        }
    }

    /// Auto-disable monitoring when the GUI stops polling. Runs for the life of
    /// the agent: each tick, if monitoring is on but no poll arrived since the
    /// previous tick, the GUI is gone — disable and free the buffer.
    pub async fn run_idle_janitor(self: SharedEventMonitor) {
        // `interval` fires its first tick immediately; `interval_at` delays the
        // first check by a full `IDLE_TICK`. That matters on an agent restart
        // while monitoring was enabled: an immediate first tick would see
        // `enabled == true` with no poll yet this window and disable before the
        // reconnecting GUI repolls. Waiting one full window lets it poll first.
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + IDLE_TICK, IDLE_TICK);
        loop {
            ticker.tick().await;
            // Acquire-load `enabled` to pair with `poll`'s Release store: seeing
            // `enabled == true` here guarantees the matching `polled = true` is
            // visible, so a monitor enabled by a poll just before this tick is
            // never torn down for a stale `polled == false`. `swap` then consumes
            // the flag — a poll since the last tick keeps monitoring alive; an
            // untouched flag means no poll happened this interval.
            if self.enabled.load(Ordering::Acquire) && !self.polled.swap(false, Ordering::Relaxed) {
                self.disable();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::ButtonId;

    #[test]
    fn records_only_while_enabled_and_skips_moves() {
        let m = EventMonitor::default();
        // Off by default: a press before any poll is not buffered.
        m.record(&MouseEvent::Button {
            id: ButtonId::Back,
            pressed: true,
        });
        assert!(!m.enabled());

        // The first poll enables monitoring and returns nothing buffered yet.
        assert!(m.poll().is_empty());
        assert!(m.enabled());

        // Now events land — except pointer moves, which are dropped.
        m.record(&MouseEvent::Moved {
            delta_x: 5,
            delta_y: 5,
        });
        m.record(&MouseEvent::Button {
            id: ButtonId::Forward,
            pressed: false,
        });
        assert_eq!(
            m.poll(),
            vec![MonitorEvent::Button {
                button: ButtonId::Forward.to_string(),
                pressed: false,
            }]
        );
        // Draining leaves the buffer empty.
        assert!(m.poll().is_empty());
    }

    #[test]
    fn bounded_buffer_drops_oldest() {
        let m = EventMonitor::default();
        m.poll(); // enable
        for _ in 0..(CAPACITY + 10) {
            m.record(&MouseEvent::Scroll {
                delta_x: 0.0,
                delta_y: 1.0,
                from_trackpad: false,
                device: None,
            });
        }
        assert_eq!(m.poll().len(), CAPACITY, "never grows past the cap");
    }
}
