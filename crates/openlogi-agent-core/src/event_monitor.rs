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
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use openlogi_hook::MouseEvent;
use openlogi_ipc::MonitorEvent;

/// A shared [`EventMonitor`], threaded between the hook callback (writer) and
/// the IPC server (reader/poller).
pub type SharedEventMonitor = std::sync::Arc<EventMonitor>;

/// How many recent events to retain between polls. A held button + a flick of
/// the scroll wheel is a handful of events; a generous cap still drops only the
/// oldest if the GUI stalls.
const CAPACITY: usize = 256;

/// How often the janitor checks for an idle (no-longer-polled) monitor.
const IDLE_TICK: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum MonitorState {
    Disabled = 0,
    Polled = 1,
    Idle = 2,
}

impl MonitorState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Disabled,
            1 => Self::Polled,
            2 => Self::Idle,
            _ => unreachable!("EventMonitor stores only MonitorState discriminants"),
        }
    }
}

/// Buffers the hook's observed events for the GUI's live monitor when enabled.
pub struct EventMonitor {
    state: AtomicU8,
    buf: Mutex<VecDeque<MonitorEvent>>,
}

impl Default for EventMonitor {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(MonitorState::Disabled as u8),
            buf: Mutex::default(),
        }
    }
}

impl EventMonitor {
    /// Whether monitoring is currently on — the one check the hot hook path runs.
    #[must_use]
    pub fn enabled(&self) -> bool {
        MonitorState::from_raw(self.state.load(Ordering::Relaxed)) != MonitorState::Disabled
    }

    /// Record a hook event, if monitoring is on. Pointer moves are dropped: they
    /// arrive at pointer-motion rates and would evict every button/scroll event
    /// from the bounded buffer before the GUI's next poll.
    pub fn record(&self, event: &MouseEvent) {
        if !self.enabled() {
            return;
        }
        let mapped = match event {
            MouseEvent::Button { id, pressed, .. } => MonitorEvent::Button {
                button: id.to_string(),
                pressed: *pressed,
            },
            MouseEvent::Scroll { delta, .. } =>
            {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the monitor wire format is diagnostic f32 data; runtime scroll keeps f64"
                )]
                MonitorEvent::Scroll {
                    delta_x: delta.x() as f32,
                    delta_y: delta.y() as f32,
                }
            }
            MouseEvent::CaptureInterrupted => MonitorEvent::CaptureInterrupted,
            MouseEvent::Moved { .. } => return,
        };
        // `try_lock` only — the freeze-sensitive hook callback must never block
        // on the monitor buffer (a contended `lock` stalls every pointer event).
        if let Ok(mut buf) = self.buf.try_lock() {
            if buf.len() == CAPACITY {
                buf.pop_front();
            }
            buf.push_back(mapped);
        }
    }

    /// Enable monitoring (idempotent) and drain everything buffered since the
    /// last poll. Called from the IPC `poll_event_monitor` handler.
    pub fn poll(&self) -> Vec<MonitorEvent> {
        self.state
            .store(MonitorState::Polled as u8, Ordering::Release);
        self.buf
            .lock()
            .map(|mut buf| buf.drain(..).collect())
            .unwrap_or_default()
    }

    fn idle_tick(&self) {
        let mut raw = self.state.load(Ordering::Acquire);
        loop {
            let current = MonitorState::from_raw(raw);
            let next = match current {
                MonitorState::Disabled => return,
                MonitorState::Polled => MonitorState::Idle,
                MonitorState::Idle => MonitorState::Disabled,
            };
            match self.state.compare_exchange_weak(
                raw,
                next as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if next == MonitorState::Disabled
                        && let Ok(mut buf) = self.buf.lock()
                        && MonitorState::from_raw(self.state.load(Ordering::Acquire))
                            == MonitorState::Disabled
                    {
                        buf.clear();
                    }
                    return;
                }
                Err(actual) => raw = actual,
            }
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
            self.idle_tick();
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
            device: None,
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
            device: None,
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
                delta: openlogi_hook::ScrollDelta::wheel_ticks(0.0, 1.0),
                from_trackpad: false,
                device: None,
            });
        }
        assert_eq!(m.poll().len(), CAPACITY, "never grows past the cap");
    }

    #[test]
    fn idle_lifecycle_requires_a_full_tick_without_a_poll_to_disable() {
        let m = EventMonitor::default();
        m.idle_tick();
        assert!(!m.enabled());

        m.poll();
        m.idle_tick();
        assert!(m.enabled(), "the first tick consumes the enabling poll");

        m.poll();
        m.idle_tick();
        assert!(m.enabled(), "a poll refreshes an idle monitor");

        m.idle_tick();
        assert!(
            !m.enabled(),
            "one whole interval without a poll disables it"
        );
    }
}
