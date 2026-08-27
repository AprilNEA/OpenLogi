//! Shared and exclusive access coordination for receiver HID++ sessions.
//!
//! Long-running HID++ sessions share pooled receiver channels under read leases.
//! Pairing and coordinated host transitions announce their intent so those
//! sessions stop, then wait for an exclusive write lease.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// Coordinates exclusive access to the receiver HID node.
#[derive(Clone, Default)]
pub struct ReceiverAccess {
    inner: Arc<ReceiverAccessInner>,
}

#[derive(Default)]
struct ReceiverAccessInner {
    lease: Arc<RwLock<()>>,
    exclusive_requests: Arc<ExclusiveRequests>,
}

#[derive(Default)]
struct ExclusiveRequests {
    total: AtomicUsize,
    pairing: AtomicUsize,
    host_transition: AtomicUsize,
}

/// Operation requiring sole ownership of a receiver transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusiveAccessReason {
    /// Receiver discovery and pairing.
    Pairing,
    /// Coordinated movement of linked devices to another host.
    HostTransition,
}

impl ExclusiveAccessReason {
    fn counter(self, requests: &ExclusiveRequests) -> &AtomicUsize {
        match self {
            Self::Pairing => &requests.pairing,
            Self::HostTransition => &requests.host_transition,
        }
    }
}

impl ExclusiveRequests {
    fn any(&self) -> bool {
        self.total.load(Ordering::Acquire) != 0
    }

    fn requested(&self, reason: ExclusiveAccessReason) -> bool {
        reason.counter(self).load(Ordering::Acquire) != 0
    }
}

/// Shared receiver lease held by a long-running HID++ session.
pub struct SessionReceiverLease {
    _guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive receiver lease held by a pairing or host-transition operation.
pub struct ExclusiveReceiverLease {
    _guard: OwnedRwLockWriteGuard<()>,
    _request: ExclusiveRequest,
}

impl ReceiverAccess {
    /// Whether any exclusive operation is waiting for or holding receiver access.
    #[must_use]
    pub fn exclusive_requested(&self) -> bool {
        self.inner.exclusive_requests.any()
    }

    /// Whether `reason` is waiting for or holding receiver access.
    #[must_use]
    pub fn requested(&self, reason: ExclusiveAccessReason) -> bool {
        self.inner.exclusive_requests.requested(reason)
    }

    /// Try to acquire receiver access for a pooled HID++ session.
    ///
    /// Capture is opportunistic: if pairing is waiting or active, capture should
    /// stay idle and retry on its next management tick.
    #[must_use]
    pub fn try_acquire_for_session(&self) -> Option<SessionReceiverLease> {
        if self.exclusive_requested() {
            return None;
        }
        let guard = Arc::clone(&self.inner.lease).try_read_owned().ok()?;
        if self.exclusive_requested() {
            return None;
        }
        Some(SessionReceiverLease { _guard: guard })
    }

    /// Wait for shared access for a bounded device-I/O operation.
    ///
    /// Unlike long-running sessions, ordinary reads and writes must not be
    /// dropped merely because an exclusive operation is queued. Tokio's fair
    /// lock ordering makes them wait behind that operation instead.
    pub async fn acquire_for_io(&self) -> SessionReceiverLease {
        let guard = Arc::clone(&self.inner.lease).read_owned().await;
        SessionReceiverLease { _guard: guard }
    }

    /// Request and acquire exclusive receiver access for `reason`.
    ///
    /// If the returned future is cancelled while waiting, the pairing request is
    /// withdrawn automatically so capture can resume.
    pub async fn acquire_exclusive(&self, reason: ExclusiveAccessReason) -> ExclusiveReceiverLease {
        let request = ExclusiveRequest::new(Arc::clone(&self.inner.exclusive_requests), reason);
        let guard = Arc::clone(&self.inner.lease).write_owned().await;
        ExclusiveReceiverLease {
            _guard: guard,
            _request: request,
        }
    }
}

struct ExclusiveRequest {
    requests: Arc<ExclusiveRequests>,
    reason: ExclusiveAccessReason,
}

impl ExclusiveRequest {
    fn new(requests: Arc<ExclusiveRequests>, reason: ExclusiveAccessReason) -> Self {
        requests.total.fetch_add(1, Ordering::AcqRel);
        reason.counter(&requests).fetch_add(1, Ordering::AcqRel);
        Self { requests, reason }
    }
}

impl Drop for ExclusiveRequest {
    fn drop(&mut self) {
        self.reason
            .counter(&self.requests)
            .fetch_sub(1, Ordering::AcqRel);
        self.requests.total.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pairing_request_blocks_new_capture_until_pairing_lease_drops() {
        let access = ReceiverAccess::default();

        let pairing = access
            .acquire_exclusive(ExclusiveAccessReason::Pairing)
            .await;

        assert!(access.requested(ExclusiveAccessReason::Pairing));
        assert!(access.exclusive_requested());
        assert!(access.try_acquire_for_session().is_none());

        drop(pairing);

        assert!(!access.exclusive_requested());
        assert!(access.try_acquire_for_session().is_some());
    }

    #[tokio::test]
    async fn pooled_sessions_share_access_before_pairing() {
        let access = ReceiverAccess::default();

        let first = access
            .try_acquire_for_session()
            .expect("fresh receiver access should grant first session lease");
        let second = access
            .try_acquire_for_session()
            .expect("pooled sessions should share receiver access");

        drop((first, second));
    }

    #[tokio::test]
    async fn cancelled_pairing_wait_withdraws_request() {
        let access = ReceiverAccess::default();
        let capture = access
            .try_acquire_for_session()
            .expect("fresh receiver access should grant capture lease");

        let waiting = tokio::spawn({
            let access = access.clone();
            async move {
                access
                    .acquire_exclusive(ExclusiveAccessReason::Pairing)
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(access.requested(ExclusiveAccessReason::Pairing));

        waiting.abort();
        let _ = waiting.await;
        assert!(!access.exclusive_requested());
        drop(capture);
        assert!(access.try_acquire_for_session().is_some());
    }

    #[test]
    fn same_reason_overlap_stays_requested_until_every_request_drops() {
        let access = ReceiverAccess::default();
        let first = ExclusiveRequest::new(
            Arc::clone(&access.inner.exclusive_requests),
            ExclusiveAccessReason::Pairing,
        );
        let second = ExclusiveRequest::new(
            Arc::clone(&access.inner.exclusive_requests),
            ExclusiveAccessReason::Pairing,
        );

        drop(first);
        assert!(access.requested(ExclusiveAccessReason::Pairing));
        assert!(access.exclusive_requested());

        drop(second);
        assert!(!access.exclusive_requested());
    }

    #[tokio::test]
    async fn host_transition_blocks_shared_sessions() {
        let access = ReceiverAccess::default();

        let transition = access
            .acquire_exclusive(ExclusiveAccessReason::HostTransition)
            .await;

        assert!(access.requested(ExclusiveAccessReason::HostTransition));
        assert!(access.try_acquire_for_session().is_none());
        drop(transition);
        assert!(access.try_acquire_for_session().is_some());
    }

    #[tokio::test]
    async fn bounded_io_waits_for_host_transition() {
        let access = ReceiverAccess::default();
        let transition = access
            .acquire_exclusive(ExclusiveAccessReason::HostTransition)
            .await;
        let waiting = tokio::spawn({
            let access = access.clone();
            async move { access.acquire_for_io().await }
        });

        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(transition);
        waiting
            .await
            .expect("bounded io must acquire its lease once the host transition releases");
    }
}
