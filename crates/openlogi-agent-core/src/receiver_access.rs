//! Shared and exclusive access coordination for receiver HID++ sessions.
//!
//! Long-running HID++ sessions share pooled receiver channels under read leases.
//! Pairing and coordinated host transitions announce their intent so those
//! sessions stop, then wait for an exclusive write lease.

use std::sync::Arc;

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock, watch};

/// Coordinates exclusive access to the receiver HID node.
#[derive(Clone)]
pub struct ReceiverAccess {
    inner: Arc<ReceiverAccessInner>,
}

struct ReceiverAccessInner {
    lease: Arc<RwLock<()>>,
    requests: watch::Sender<ReceiverRequestState>,
}

/// Authoritative count of queued or active exclusive receiver requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiverRequestState {
    pairing: usize,
    host_transition: usize,
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
    fn count_mut(self, requests: &mut ReceiverRequestState) -> &mut usize {
        match self {
            Self::Pairing => &mut requests.pairing,
            Self::HostTransition => &mut requests.host_transition,
        }
    }
}

impl ReceiverRequestState {
    /// Whether any exclusive operation is queued or active.
    #[must_use]
    pub fn any(self) -> bool {
        self.pairing != 0 || self.host_transition != 0
    }

    /// Whether an operation for `reason` is queued or active.
    #[must_use]
    pub fn requested(self, reason: ExclusiveAccessReason) -> bool {
        match reason {
            ExclusiveAccessReason::Pairing => self.pairing != 0,
            ExclusiveAccessReason::HostTransition => self.host_transition != 0,
        }
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

impl Default for ReceiverAccess {
    fn default() -> Self {
        let (requests, _) = watch::channel(ReceiverRequestState::default());
        Self {
            inner: Arc::new(ReceiverAccessInner {
                lease: Arc::new(RwLock::new(())),
                requests,
            }),
        }
    }
}

impl ReceiverAccess {
    /// Whether any exclusive operation is waiting for or holding receiver access.
    #[must_use]
    pub fn exclusive_requested(&self) -> bool {
        self.request_state().any()
    }

    /// Whether `reason` is waiting for or holding receiver access.
    #[must_use]
    pub fn requested(&self, reason: ExclusiveAccessReason) -> bool {
        self.request_state().requested(reason)
    }

    /// Subscribe to requested-state changes, starting with the current counts.
    #[must_use]
    pub fn subscribe_requests(&self) -> watch::Receiver<ReceiverRequestState> {
        self.inner.requests.subscribe()
    }

    /// Try to acquire receiver access for a pooled HID++ session.
    ///
    /// Capture is opportunistic: if pairing is waiting or active, capture should
    /// stay idle until the next requested-state reconciliation.
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
        let request = ExclusiveRequest::new(self.inner.requests.clone(), reason);
        let guard = Arc::clone(&self.inner.lease).write_owned().await;
        ExclusiveReceiverLease {
            _guard: guard,
            _request: request,
        }
    }

    fn request_state(&self) -> ReceiverRequestState {
        *self.inner.requests.borrow()
    }
}

struct ExclusiveRequest {
    requests: watch::Sender<ReceiverRequestState>,
    reason: ExclusiveAccessReason,
}

impl ExclusiveRequest {
    fn new(requests: watch::Sender<ReceiverRequestState>, reason: ExclusiveAccessReason) -> Self {
        requests.send_if_modified(|state| {
            let count = reason.count_mut(state);
            *count += 1;
            true
        });
        Self { requests, reason }
    }
}

impl Drop for ExclusiveRequest {
    fn drop(&mut self) {
        self.requests.send_if_modified(|state| {
            let count = self.reason.count_mut(state);
            debug_assert!(*count != 0, "every request drop has a matching begin");
            *count -= 1;
            true
        });
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
            access.inner.requests.clone(),
            ExclusiveAccessReason::Pairing,
        );
        let second = ExclusiveRequest::new(
            access.inner.requests.clone(),
            ExclusiveAccessReason::Pairing,
        );

        drop(first);
        assert!(access.requested(ExclusiveAccessReason::Pairing));
        assert!(access.exclusive_requested());

        drop(second);
        assert!(!access.exclusive_requested());
    }

    #[tokio::test]
    async fn request_begin_and_end_publish_immediately() {
        let access = ReceiverAccess::default();
        let mut requests = access.subscribe_requests();

        let request = ExclusiveRequest::new(
            access.inner.requests.clone(),
            ExclusiveAccessReason::Pairing,
        );
        requests
            .changed()
            .await
            .expect("request publication should remain open");
        assert!(
            requests
                .borrow_and_update()
                .requested(ExclusiveAccessReason::Pairing)
        );

        drop(request);
        requests
            .changed()
            .await
            .expect("request publication should remain open");
        assert!(!requests.borrow_and_update().any());
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
