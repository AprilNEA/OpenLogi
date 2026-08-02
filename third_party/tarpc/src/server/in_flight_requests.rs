use crate::util::{Compact, TimeUntil};
use fnv::FnvHashMap;
use futures::future::{AbortHandle, AbortRegistration};
use std::{
    collections::hash_map,
    task::{Context, Poll},
    time::Instant,
};
use tokio_util::time::delay_queue::{self, DelayQueue};
use tracing::Span;

/// A data structure that tracks in-flight requests. It aborts requests,
/// either on demand or when a request deadline expires.
#[derive(Debug, Default)]
pub struct InFlightRequests {
    request_data: FnvHashMap<u64, RequestData>,
    deadlines: DelayQueue<u64>,
}

/// Data needed to clean up a single in-flight request.
#[derive(Debug)]
struct RequestData {
    /// Aborts the response handler for the associated request.
    abort_handle: AbortHandle,
    /// The key to remove the timer for the request's deadline.
    deadline_key: delay_queue::Key,
    /// The client span.
    span: Span,
}

/// An error returned when a request attempted to start with the same ID as a request already
/// in flight.
#[derive(Debug)]
pub struct AlreadyExistsError;

impl InFlightRequests {
    /// Returns the number of in-flight requests.
    pub fn len(&self) -> usize {
        self.request_data.len()
    }

    /// Starts a request, unless a request with the same ID is already in flight.
    pub fn start_request(
        &mut self,
        request_id: u64,
        deadline: Instant,
        span: Span,
    ) -> Result<AbortRegistration, AlreadyExistsError> {
        match self.request_data.entry(request_id) {
            hash_map::Entry::Vacant(vacant) => {
                let timeout = deadline.time_until();
                let (abort_handle, abort_registration) = AbortHandle::new_pair();
                let deadline_key = self.deadlines.insert(request_id, timeout);
                vacant.insert(RequestData {
                    abort_handle,
                    deadline_key,
                    span,
                });
                Ok(abort_registration)
            }
            hash_map::Entry::Occupied(_) => Err(AlreadyExistsError),
        }
    }

    /// Cancels an in-flight request. Returns true iff the request was found.
    pub fn cancel_request(&mut self, request_id: u64) -> bool {
        if let Some(RequestData {
            span,
            abort_handle,
            deadline_key,
        }) = self.request_data.remove(&request_id)
        {
            let _entered = span.enter();
            self.request_data.compact(0.1);
            abort_handle.abort();
            self.deadlines.remove(&deadline_key);
            tracing::info!("ReceiveCancel");
            true
        } else {
            false
        }
    }

    /// Removes a request without aborting. Returns true iff the request was found.
    /// This method should be used when a response is being sent.
    pub fn remove_request(&mut self, request_id: u64) -> Option<Span> {
        if let Some(request_data) = self.request_data.remove(&request_id) {
            self.request_data.compact(0.1);
            self.deadlines.remove(&request_data.deadline_key);
            Some(request_data.span)
        } else {
            None
        }
    }

    /// Yields a request that has expired, aborting any ongoing processing of that request.
    pub fn poll_expired(&mut self, cx: &mut Context) -> Poll<Option<u64>> {
        if self.deadlines.is_empty() {
            // TODO(https://github.com/tokio-rs/tokio/issues/4161)
            // This is a workaround for DelayQueue not always treating this case correctly.
            return Poll::Ready(None);
        }
        self.deadlines.poll_expired(cx).map(|expired| {
            let expired = expired?;
            if let Some(RequestData {
                abort_handle, span, ..
            }) = self.request_data.remove(expired.get_ref())
            {
                let _entered = span.enter();
                self.request_data.compact(0.1);
                abort_handle.abort();
                tracing::error!("DeadlineExceeded");
            }
            Some(expired.into_inner())
        })
    }
}

/// When InFlightRequests is dropped, any outstanding requests are aborted.
impl Drop for InFlightRequests {
    fn drop(&mut self) {
        self.request_data
            .values()
            .for_each(|request_data| request_data.abort_handle.abort())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use futures::{
        future::{pending, Abortable},
        FutureExt,
    };
    use futures_test::task::noop_context;

    #[tokio::test]
    async fn start_request_increases_len() {
        let mut in_flight_requests = InFlightRequests::default();
        assert_eq!(in_flight_requests.len(), 0);
        in_flight_requests
            .start_request(0, Instant::now(), Span::current())
            .unwrap();
        assert_eq!(in_flight_requests.len(), 1);
    }

    #[tokio::test]
    async fn polling_expired_aborts() {
        let mut in_flight_requests = InFlightRequests::default();
        let abort_registration = in_flight_requests
            .start_request(0, Instant::now(), Span::current())
            .unwrap();
        let mut abortable_future = Box::new(Abortable::new(pending::<()>(), abort_registration));

        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(1000)).await;

        assert_matches!(
            in_flight_requests.poll_expired(&mut noop_context()),
            Poll::Ready(Some(_))
        );
        assert_matches!(
            abortable_future.poll_unpin(&mut noop_context()),
            Poll::Ready(Err(_))
        );
        assert_eq!(in_flight_requests.len(), 0);
    }

    #[tokio::test]
    async fn cancel_request_aborts() {
        let mut in_flight_requests = InFlightRequests::default();
        let abort_registration = in_flight_requests
            .start_request(0, Instant::now(), Span::current())
            .unwrap();
        let mut abortable_future = Box::new(Abortable::new(pending::<()>(), abort_registration));

        assert!(in_flight_requests.cancel_request(0));
        assert_matches!(
            abortable_future.poll_unpin(&mut noop_context()),
            Poll::Ready(Err(_))
        );
        assert_eq!(in_flight_requests.len(), 0);
    }

    #[tokio::test]
    async fn remove_request_doesnt_abort() {
        let mut in_flight_requests = InFlightRequests::default();
        assert!(in_flight_requests.deadlines.is_empty());

        let abort_registration = in_flight_requests
            .start_request(
                0,
                Instant::now() + std::time::Duration::from_secs(10),
                Span::current(),
            )
            .unwrap();
        let mut abortable_future = Box::new(Abortable::new(pending::<()>(), abort_registration));

        // Precondition: Pending expiration
        assert_matches!(
            in_flight_requests.poll_expired(&mut noop_context()),
            Poll::Pending
        );
        assert!(!in_flight_requests.deadlines.is_empty());

        assert_matches!(in_flight_requests.remove_request(0), Some(_));
        // Postcondition: No pending expirations
        assert!(in_flight_requests.deadlines.is_empty());
        assert_matches!(
            in_flight_requests.poll_expired(&mut noop_context()),
            Poll::Ready(None)
        );
        assert_matches!(
            abortable_future.poll_unpin(&mut noop_context()),
            Poll::Pending
        );
        assert_eq!(in_flight_requests.len(), 0);
    }
}
