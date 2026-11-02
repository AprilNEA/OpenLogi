//! Live event monitor: a shared, bounded buffer that mirrors the events the OS
//! mouse hook observes to the GUI's debug monitor, on demand.
//!
//! Monitoring is **off by default**. The freeze-sensitive hook callback pays
//! only a single relaxed atomic load per event while off (see the freeze-hazard
//! note in `openlogi-hook`); it locks and pushes only once the GUI starts
//! polling. The GUI enables monitoring implicitly by polling
//! [`EventMonitor::poll`], which also starts an idle janitor that turns it back
//! off when polls stop — so a closed panel or a crashed GUI can't leave the
//! callback doing buffer work forever. The janitor exits with the monitor, so
//! an agent nobody monitors runs no timer for it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
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
const IDLE_CHECK_PERIOD: Duration = Duration::from_secs(3);

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
    /// Whether an idle janitor task is running for this monitor.
    janitor: AtomicBool,
}

impl Default for EventMonitor {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(MonitorState::Disabled as u8),
            buf: Mutex::default(),
            janitor: AtomicBool::new(false),
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

    /// Enable monitoring (idempotent), start the idle janitor if none runs, and
    /// drain everything buffered since the last poll. Called from the IPC
    /// `poll_event_monitor` handler; outside a Tokio runtime no janitor starts.
    pub fn poll(self: &SharedEventMonitor) -> Vec<MonitorEvent> {
        self.state
            .store(MonitorState::Polled as u8, Ordering::SeqCst);
        self.ensure_janitor();
        self.buf
            .lock()
            .map(|mut buf| buf.drain(..).collect())
            .unwrap_or_default()
    }

    fn ensure_janitor(self: &SharedEventMonitor) {
        if self.janitor.swap(true, Ordering::SeqCst) {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(Arc::clone(self).run_idle_janitor());
            }
            Err(_) => self.janitor.store(false, Ordering::SeqCst),
        }
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

    /// Auto-disable monitoring when the GUI stops polling: each tick, if no
    /// poll arrived since the previous one, the GUI is gone — disable and free
    /// the buffer, then exit. The next poll starts a fresh janitor.
    async fn run_idle_janitor(self: SharedEventMonitor) {
        // `interval` fires its first tick immediately; `interval_at` gives the
        // poll that started this janitor a full `IDLE_CHECK_PERIOD` before the
        // first check, the same window every later poll gets.
        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + IDLE_CHECK_PERIOD,
            IDLE_CHECK_PERIOD,
        );
        loop {
            ticker.tick().await;
            self.idle_tick();
            if self.enabled() {
                continue;
            }
            // Retire, unless a poll re-enabled monitoring after the tick
            // disabled it. With every access here and in `poll` sequentially
            // consistent, either that poll sees the cleared flag and starts a
            // janitor, or this one sees its state and carries on — never
            // neither.
            self.janitor.store(false, Ordering::SeqCst);
            if MonitorState::from_raw(self.state.load(Ordering::SeqCst)) == MonitorState::Disabled
                || self.janitor.swap(true, Ordering::SeqCst)
            {
                return;
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
        let m = Arc::new(EventMonitor::default());
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
        let m = Arc::new(EventMonitor::default());
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
        let m = Arc::new(EventMonitor::default());
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

    /// The janitor runs only while monitoring is on: a poll starts it, an
    /// interval without a poll disables monitoring and ends it, and the next
    /// poll starts a fresh one.
    #[tokio::test(start_paused = true)]
    async fn janitor_runs_only_while_monitoring() {
        let m = Arc::new(EventMonitor::default());
        assert!(!m.janitor.load(Ordering::SeqCst), "no timer before a poll");

        m.poll();
        assert!(m.janitor.load(Ordering::SeqCst));
        // Polled → Idle on the first tick, Idle → Disabled on the second.
        tokio::time::sleep(IDLE_CHECK_PERIOD * 2 + Duration::from_millis(1)).await;
        assert!(!m.enabled());
        assert!(
            !m.janitor.load(Ordering::SeqCst),
            "the janitor exits with the monitor"
        );

        m.poll();
        assert!(m.enabled());
        assert!(m.janitor.load(Ordering::SeqCst), "a new poll restarts it");
    }
}
