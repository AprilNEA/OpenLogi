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
#[derive(Clone)]
pub struct DeviceIoGate {
    receiver: watch::Receiver<DeviceIoState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeviceIoState {
    Allowed,
    Suspended,
}

/// Create an initially-open device-I/O gate.
#[must_use]
pub fn device_io_channel() -> (DeviceIoSignal, DeviceIoGate) {
    let (sender, receiver) = watch::channel(DeviceIoState::Allowed);
    (DeviceIoSignal { sender }, DeviceIoGate { receiver })
}

impl DeviceIoSignal {
    /// Close the gate without blocking the native lifecycle callback.
    ///
    /// Returns whether this call changed the published state.
    #[must_use]
    pub fn suspend(&self) -> bool {
        self.sender.send_if_modified(|state| {
            if *state == DeviceIoState::Suspended {
                return false;
            }
            *state = DeviceIoState::Suspended;
            true
        })
    }

    /// Reopen the gate after a user-visible resume.
    ///
    /// Returns whether this call changed the published state.
    #[must_use]
    pub fn resume(&self) -> bool {
        self.sender.send_if_modified(|state| {
            if *state == DeviceIoState::Allowed {
                return false;
            }
            *state = DeviceIoState::Allowed;
            true
        })
    }
}

impl DeviceIoGate {
    /// Whether opening a node or sending a request is currently allowed.
    #[must_use]
    pub fn allows_io(&self) -> bool {
        *self.receiver.borrow() == DeviceIoState::Allowed
    }

    /// Wait for the next distinct gate transition.
    ///
    /// Returns the new allow-state, or `None` if the lifecycle producer was
    /// dropped.
    pub async fn changed(&mut self) -> Option<bool> {
        self.receiver.changed().await.ok()?;
        Some(self.allows_io())
    }

    /// Wait until the gate is open. Returns `false` if its producer disappears
    /// while it is closed.
    pub async fn wait_until_allowed(&mut self) -> bool {
        while !self.allows_io() {
            if self.changed().await.is_none() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::device_io_channel;

    #[tokio::test]
    async fn latest_transition_is_retained_and_duplicates_coalesce() {
        let (signal, mut gate) = device_io_channel();
        assert!(gate.allows_io());

        assert!(signal.suspend());
        assert!(!signal.suspend());
        gate.changed()
            .await
            .expect("the suspend transition should remain available");
        assert!(!gate.allows_io());
        tokio::time::timeout(Duration::from_millis(10), gate.changed())
            .await
            .expect_err("duplicate suspend publications must coalesce");

        assert!(signal.resume());
        assert!(gate.wait_until_allowed().await);
        assert!(gate.allows_io());
    }
}
