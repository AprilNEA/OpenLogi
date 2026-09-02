//! Process activity gate for host HID access.
//!
//! The gate carries policy from a host lifecycle observer down to code that
//! may open a HID node or send a request. It contains no host integration of
//! its own, so the device layer remains portable and testable.

use tokio::sync::watch;

/// Non-blocking producer owned by the host lifecycle observer.
#[derive(Clone)]
pub struct DeviceIoSignal {
    sender: watch::Sender<DeviceIoState>,
}

/// Cheaply cloneable read capability for device-I/O producers.
pub struct DeviceIoGate {
    receiver: watch::Receiver<DeviceIoState>,
    /// The state represented by the last transition returned to this reader.
    ///
    /// `watch` retains only the latest value. The generation lets a reader
    /// replay each alternating edge when multiple transitions arrive before it
    /// gets polled, so a suspend edge cannot disappear behind a resume edge.
    observed: DeviceIoState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeviceIoState {
    allowed: bool,
    generation: u64,
}

/// Create an initially-open device-I/O gate.
#[must_use]
pub fn device_io_channel() -> (DeviceIoSignal, DeviceIoGate) {
    let initial = DeviceIoState {
        allowed: true,
        generation: 0,
    };
    let (sender, receiver) = watch::channel(initial);
    (
        DeviceIoSignal { sender },
        DeviceIoGate {
            receiver,
            observed: initial,
        },
    )
}

impl Clone for DeviceIoGate {
    fn clone(&self) -> Self {
        let receiver = self.receiver.clone();
        // A new consumer subscribes to the current policy, not to the source
        // reader's already-delivered history.
        let observed = *receiver.borrow();
        Self { receiver, observed }
    }
}

impl DeviceIoSignal {
    /// Close the gate without blocking the native lifecycle callback.
    ///
    /// Returns whether this call changed the published state.
    #[must_use]
    pub fn suspend(&self) -> bool {
        self.sender.send_if_modified(|state| {
            if !state.allowed {
                return false;
            }
            state.allowed = false;
            state.generation = state.generation.wrapping_add(1);
            true
        })
    }

    /// Reopen the gate after a user-visible resume.
    ///
    /// Returns whether this call changed the published state.
    #[must_use]
    pub fn resume(&self) -> bool {
        self.sender.send_if_modified(|state| {
            if state.allowed {
                return false;
            }
            state.allowed = true;
            state.generation = state.generation.wrapping_add(1);
            true
        })
    }
}

impl DeviceIoGate {
    /// Whether opening a node or sending a request is currently allowed.
    #[must_use]
    pub fn allows_io(&self) -> bool {
        self.receiver.borrow().allowed
    }

    /// Wait for the next distinct gate transition, in order.
    ///
    /// The underlying watch channel retains only the latest state. A reader
    /// that falls behind therefore replays the alternating transitions from
    /// its local generation cursor before waiting for a new publication. This
    /// preserves a suspend edge even when a resume follows it immediately.
    /// Returns `None` if the lifecycle producer was dropped after all observed
    /// transitions have been delivered.
    pub async fn changed(&mut self) -> Option<bool> {
        loop {
            let latest = *self.receiver.borrow();
            if latest.generation != self.observed.generation {
                let allowed = !self.observed.allowed;
                self.observed = DeviceIoState {
                    allowed,
                    generation: self.observed.generation.wrapping_add(1),
                };
                return Some(allowed);
            }
            self.receiver.changed().await.ok()?;
        }
    }

    /// Mark the currently published state as observed, discarding any older
    /// transitions that a caller has already reconciled from [`Self::allows_io`].
    pub fn synchronize(&mut self) {
        self.observed = *self.receiver.borrow_and_update();
    }

    /// Wait until the gate is open. Returns `false` if its producer disappears
    /// while it is closed. On success, stale transitions are folded into the
    /// current open state so the next [`Self::changed`] waits for a new edge.
    pub async fn wait_until_allowed(&mut self) -> bool {
        while !self.allows_io() {
            if self.changed().await.is_none() {
                return false;
            }
        }
        self.synchronize();
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::device_io_channel;

    #[tokio::test]
    async fn transitions_are_replayed_and_duplicates_coalesce() {
        let (signal, mut gate) = device_io_channel();
        assert!(gate.allows_io());

        assert!(signal.suspend());
        assert!(!signal.suspend());
        assert!(signal.resume());
        assert!(!signal.resume());

        assert_eq!(gate.changed().await, Some(false));
        assert_eq!(gate.changed().await, Some(true));
        tokio::time::timeout(Duration::from_millis(10), gate.changed())
            .await
            .expect_err("duplicate state publications must coalesce");
        assert!(gate.allows_io());
    }

    #[tokio::test]
    async fn wait_until_allowed_folds_a_fast_resume_into_current_state() {
        let (signal, mut gate) = device_io_channel();
        assert!(signal.suspend());
        assert!(signal.resume());

        assert!(gate.wait_until_allowed().await);
        tokio::time::timeout(Duration::from_millis(10), gate.changed())
            .await
            .expect_err("wait_until_allowed should consume stale transitions");
    }

    #[tokio::test]
    async fn cloned_gate_starts_at_the_current_state() {
        let (signal, gate) = device_io_channel();
        assert!(signal.suspend());

        let mut clone = gate.clone();
        assert!(!clone.allows_io());
        tokio::time::timeout(Duration::from_millis(10), clone.changed())
            .await
            .expect_err("a new gate should not replay pre-subscription history");

        assert!(signal.resume());
        assert_eq!(clone.changed().await, Some(true));
    }
}
