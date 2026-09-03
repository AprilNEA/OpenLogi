//! Bounded diagnostic mirror of the Agent's existing raw-touchpad capture
//! sessions.
//!
//! Monitoring is off until an IPC client polls. Capture keeps its single HID++
//! channel and watcher; this module only mirrors normalized events after they
//! have already arrived at the capture manager.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use openlogi_hid::CapturedInput;
use openlogi_ipc::{
    TouchpadMonitorBatch, TouchpadMonitorContact, TouchpadMonitorEvent, TouchpadMonitorRecord,
    TouchpadRawModeConflict,
};
use tokio::sync::watch;

/// Shared monitor used by the capture manager and Agent IPC server.
pub type SharedTouchpadMonitor = Arc<TouchpadMonitor>;

/// Enough for several seconds at touchpad report cadence even if a diagnostic
/// client briefly stalls, without allowing an absent client to grow memory.
const CAPACITY: usize = 2_048;
const IDLE_TICK: Duration = Duration::from_secs(3);

#[derive(Default)]
struct MonitorState {
    requested_device: Option<String>,
    events: VecDeque<TouchpadMonitorRecord>,
    dropped_events: u64,
    conflicts: BTreeMap<String, TouchpadRawModeConflict>,
}

/// On-demand trace buffer for normalized `0x6100` capture events.
pub struct TouchpadMonitor {
    polled: AtomicBool,
    state: Mutex<MonitorState>,
    capture_requests: watch::Sender<()>,
}

impl Default for TouchpadMonitor {
    fn default() -> Self {
        let (capture_requests, _) = watch::channel(());
        Self {
            polled: AtomicBool::new(false),
            state: Mutex::new(MonitorState::default()),
            capture_requests,
        }
    }
}

impl TouchpadMonitor {
    /// Subscribe to changes in the device requested for diagnostic capture.
    pub(crate) fn subscribe_capture_requests(&self) -> watch::Receiver<()> {
        self.capture_requests.subscribe()
    }

    /// Whether an active diagnostic requests raw-touchpad capture for this
    /// exact stable device in its existing session.
    #[must_use]
    pub fn capture_requested_for(&self, device_key: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requested_device
            .as_deref()
            == Some(device_key)
    }

    /// Mirror one input from a current capture session when diagnostics are
    /// enabled. Non-touchpad inputs are ignored.
    pub fn record(&self, device_key: &str, input: &CapturedInput) {
        let event = match input {
            CapturedInput::TouchpadFrame(frame) => TouchpadMonitorEvent::Frame {
                timestamp_us: frame.timestamp_us,
                button: frame.button,
                contacts: frame
                    .contacts()
                    .iter()
                    .map(|contact| TouchpadMonitorContact {
                        id: contact.id,
                        x_um: contact.x_um,
                        y_um: contact.y_um,
                    })
                    .collect(),
            },
            CapturedInput::TouchpadEnd => TouchpadMonitorEvent::End,
            CapturedInput::TouchpadCancel => TouchpadMonitorEvent::Cancel,
            CapturedInput::TouchpadDroppedFrames(count) => {
                TouchpadMonitorEvent::DroppedFrames { count: *count }
            }
            CapturedInput::Gesture(_, _)
            | CapturedInput::ButtonDown(_)
            | CapturedInput::Scroll { .. }
            | CapturedInput::ThumbwheelDirection { .. }
            | CapturedInput::ButtonUp(_)
            | CapturedInput::ButtonPulse(_) => return,
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.requested_device.as_deref() != Some(device_key) {
            return;
        }
        if state.events.len() == CAPACITY {
            state.events.pop_front();
            state.dropped_events = state.dropped_events.saturating_add(1);
        }
        state.events.push_back(TouchpadMonitorRecord {
            device_key: device_key.to_string(),
            event,
        });
    }

    /// Publish a conflict even while event buffering is off, so a diagnostic
    /// started after the takeover can still explain why capture is blocked.
    pub fn set_conflict(&self, device_key: &str, expected: u8, actual: u8) {
        let conflict = TouchpadRawModeConflict {
            device_key: device_key.to_string(),
            expected,
            actual,
        };
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .conflicts
            .insert(device_key.to_string(), conflict);
    }

    /// Clear a conflict when that device's capture plan changes or disappears.
    pub fn clear_conflict(&self, device_key: &str) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .conflicts
            .remove(device_key);
    }

    /// Enable monitoring and drain events accumulated since the prior poll.
    /// Active conflict state is included without being consumed.
    pub fn poll(&self, device_key: &str) -> TouchpadMonitorBatch {
        self.polled.store(true, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = state.requested_device.as_deref() != Some(device_key);
        if changed {
            state.requested_device = Some(device_key.to_string());
            state.events.clear();
            state.dropped_events = 0;
        }
        let batch = TouchpadMonitorBatch {
            events: state.events.drain(..).collect(),
            dropped_events: std::mem::take(&mut state.dropped_events),
            conflicts: state
                .conflicts
                .get(device_key)
                .cloned()
                .into_iter()
                .collect(),
        };
        drop(state);
        if changed {
            self.capture_requests.send_replace(());
        }
        batch
    }

    fn disable(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = state.requested_device.take().is_some();
        state.events.clear();
        state.dropped_events = 0;
        drop(state);
        if changed {
            self.capture_requests.send_replace(());
        }
    }

    /// Disable event mirroring when diagnostic polls stop. Conflict state is
    /// retained because it describes why the capture manager remains blocked.
    pub async fn run_idle_janitor(self: SharedTouchpadMonitor) {
        let mut ticker =
            tokio::time::interval_at(tokio::time::Instant::now() + IDLE_TICK, IDLE_TICK);
        loop {
            ticker.tick().await;
            if !self.polled.swap(false, Ordering::Relaxed) {
                self.disable();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::touchpad::{TouchContact, TouchFrame};

    use super::*;

    fn frame() -> CapturedInput {
        CapturedInput::TouchpadFrame(
            TouchFrame::new(
                8_000,
                false,
                vec![TouchContact {
                    id: 2,
                    x_um: 10_000,
                    y_um: 20_000,
                }],
            )
            .expect("one contact is valid"),
        )
    }

    #[test]
    fn records_only_touchpad_inputs_after_the_first_poll() {
        let monitor = TouchpadMonitor::default();
        monitor.record("unit:casa", &frame());
        assert!(monitor.poll("unit:casa").events.is_empty());
        assert!(monitor.capture_requested_for("unit:casa"));
        assert!(!monitor.capture_requested_for("unit:other"));

        monitor.record("unit:casa", &frame());
        monitor.record("unit:other", &frame());

        assert_eq!(
            monitor.poll("unit:casa").events,
            vec![TouchpadMonitorRecord {
                device_key: "unit:casa".into(),
                event: TouchpadMonitorEvent::Frame {
                    timestamp_us: 8_000,
                    button: false,
                    contacts: vec![TouchpadMonitorContact {
                        id: 2,
                        x_um: 10_000,
                        y_um: 20_000,
                    }],
                },
            }]
        );
    }

    #[test]
    fn bounded_buffer_reports_evicted_events() {
        let monitor = TouchpadMonitor::default();
        monitor.poll("unit:casa");
        for _ in 0..=CAPACITY {
            monitor.record("unit:casa", &CapturedInput::TouchpadEnd);
        }

        let batch = monitor.poll("unit:casa");
        assert_eq!(batch.events.len(), CAPACITY);
        assert_eq!(batch.dropped_events, 1);
    }

    #[test]
    fn conflicts_survive_disabled_event_buffering_until_cleared() {
        let monitor = TouchpadMonitor::default();
        monitor.set_conflict("unit:casa", 0x05, 0x00);

        assert_eq!(
            monitor.poll("unit:casa").conflicts,
            vec![TouchpadRawModeConflict {
                device_key: "unit:casa".into(),
                expected: 0x05,
                actual: 0x00,
            }]
        );
        monitor.clear_conflict("unit:casa");
        assert!(monitor.poll("unit:casa").conflicts.is_empty());
    }

    #[tokio::test]
    async fn capture_request_changes_notify_the_capture_manager() {
        let monitor = TouchpadMonitor::default();
        let mut requests = monitor.subscribe_capture_requests();

        monitor.poll("unit:casa");
        requests
            .changed()
            .await
            .expect("the capture request publisher should remain open");
        monitor.poll("unit:casa");
        assert!(
            !requests
                .has_changed()
                .expect("the capture request publisher should remain open")
        );

        monitor.poll("unit:other");
        requests
            .changed()
            .await
            .expect("changing the requested device should publish");
        monitor.disable();
        requests
            .changed()
            .await
            .expect("disabling diagnostics should publish");
        assert!(!monitor.capture_requested_for("unit:other"));
    }
}
