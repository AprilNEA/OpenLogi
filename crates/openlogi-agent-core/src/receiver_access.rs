//! Exclusive receiver access coordination between HID++ capture and pairing.
//!
//! Long-running HID++ sessions share pooled receiver channels under read leases.
//! Pairing first announces its intent so those sessions stop, then waits for an
//! exclusive write lease before opening the receiver itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// Coordinates exclusive access to the receiver HID node.
#[derive(Clone, Default)]
pub struct ReceiverAccess {
    inner: Arc<ReceiverAccessInner>,
}

#[derive(Default)]
struct ReceiverAccessInner {
    lease: Arc<RwLock<()>>,
    pairing_requested: Arc<AtomicBool>,
}

/// Shared receiver lease held by a long-running HID++ session.
pub struct SessionReceiverLease {
    _guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive receiver lease held by a pairing session.
pub struct PairingReceiverLease {
    _guard: OwnedRwLockWriteGuard<()>,
    pairing_requested: Arc<AtomicBool>,
}

impl Drop for PairingReceiverLease {
    fn drop(&mut self) {
        self.pairing_requested.store(false, Ordering::Release);
    }
}

impl ReceiverAccess {
    /// Whether a pairing session is waiting for or holding receiver access.
    #[must_use]
    pub fn pairing_requested(&self) -> bool {
        self.inner.pairing_requested.load(Ordering::Acquire)
    }

    /// Try to acquire receiver access for a pooled HID++ session.
    ///
    /// Capture is opportunistic: if pairing is waiting or active, capture should
    /// stay idle and retry on its next management tick.
    #[must_use]
    pub fn try_acquire_for_session(&self) -> Option<SessionReceiverLease> {
        if self.pairing_requested() {
            return None;
        }
        let guard = Arc::clone(&self.inner.lease).try_read_owned().ok()?;
        if self.pairing_requested() {
            return None;
        }
        Some(SessionReceiverLease { _guard: guard })
    }

    /// Request and acquire exclusive receiver access for pairing.
    ///
    /// If the returned future is cancelled while waiting, the pairing request is
    /// withdrawn automatically so capture can resume.
    pub async fn acquire_for_pairing(&self) -> PairingReceiverLease {
        let request = PairingRequest::new(Arc::clone(&self.inner.pairing_requested));
        let guard = Arc::clone(&self.inner.lease).write_owned().await;
        request.disarm();
        PairingReceiverLease {
            _guard: guard,
            pairing_requested: Arc::clone(&self.inner.pairing_requested),
        }
    }
}

struct PairingRequest {
    pairing_requested: Arc<AtomicBool>,
    armed: bool,
}

impl PairingRequest {
    fn new(pairing_requested: Arc<AtomicBool>) -> Self {
        pairing_requested.store(true, Ordering::Release);
        Self {
            pairing_requested,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PairingRequest {
    fn drop(&mut self) {
        if self.armed {
            self.pairing_requested.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pairing_request_blocks_new_capture_until_pairing_lease_drops() {
        let access = ReceiverAccess::default();

        let pairing = access.acquire_for_pairing().await;

        assert!(access.pairing_requested());
        assert!(access.try_acquire_for_session().is_none());

        drop(pairing);

        assert!(!access.pairing_requested());
        assert!(access.try_acquire_for_session().is_some());
    }

    #[tokio::test]
    async fn pooled_sessions_share_access_before_pairing() {
        let access = ReceiverAccess::default();

        let first = access.try_acquire_for_session().unwrap_or_else(|| {
            panic!("fresh receiver access should grant first session lease");
        });
        let second = access.try_acquire_for_session().unwrap_or_else(|| {
            panic!("pooled sessions should share receiver access");
        });

        drop((first, second));
    }

    #[tokio::test]
    async fn cancelled_pairing_wait_withdraws_request() {
        let access = ReceiverAccess::default();
        let capture = access.try_acquire_for_session().unwrap_or_else(|| {
            panic!("fresh receiver access should grant capture lease");
        });

        let waiting = tokio::spawn({
            let access = access.clone();
            async move { access.acquire_for_pairing().await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(access.pairing_requested());

        waiting.abort();
        let _ = waiting.await;
        assert!(!access.pairing_requested());
        drop(capture);
        assert!(access.try_acquire_for_session().is_some());
    }
}
