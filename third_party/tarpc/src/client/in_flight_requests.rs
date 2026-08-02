use crate::{
    context,
    util::{Compact, TimeUntil},
};
use fnv::FnvHashMap;
use std::{
    collections::hash_map,
    task::{Context, Poll},
};
use tokio::sync::oneshot;
use tokio_util::time::delay_queue::{self, DelayQueue};
use tracing::Span;

/// Requests already written to the wire that haven't yet received responses.
#[derive(Debug)]
pub struct InFlightRequests<Resp> {
    request_data: FnvHashMap<u64, RequestData<Resp>>,
    deadlines: DelayQueue<u64>,
}

impl<Resp> Default for InFlightRequests<Resp> {
    fn default() -> Self {
        Self {
            request_data: Default::default(),
            deadlines: Default::default(),
        }
    }
}

#[derive(Debug)]
struct RequestData<Res> {
    ctx: context::Context,
    span: Span,
    response_completion: oneshot::Sender<Res>,
    /// The key to remove the timer for the request's deadline.
    deadline_key: delay_queue::Key,
}

/// An error returned when an attempt is made to insert a request with an ID that is already in
/// use.
#[derive(Debug)]
pub struct AlreadyExistsError;

impl<Res> InFlightRequests<Res> {
    /// Returns the number of in-flight requests.
    pub fn len(&self) -> usize {
        self.request_data.len()
    }

    /// Returns true iff there are no requests in flight.
    pub fn is_empty(&self) -> bool {
        self.request_data.is_empty()
    }

    /// Starts a request, unless a request with the same ID is already in flight.
    pub fn insert_request(
        &mut self,
        request_id: u64,
        ctx: context::Context,
        span: Span,
        response_completion: oneshot::Sender<Res>,
    ) -> Result<(), AlreadyExistsError> {
        match self.request_data.entry(request_id) {
            hash_map::Entry::Vacant(vacant) => {
                let timeout = ctx.deadline.time_until();
                let deadline_key = self.deadlines.insert(request_id, timeout);
                vacant.insert(RequestData {
                    ctx,
                    span,
                    response_completion,
                    deadline_key,
                });
                Ok(())
            }
            hash_map::Entry::Occupied(_) => Err(AlreadyExistsError),
        }
    }

    /// Removes a request without aborting. Returns true iff the request was found.
    pub fn complete_request(&mut self, request_id: u64, result: Res) -> Option<Span> {
        if let Some(request_data) = self.request_data.remove(&request_id) {
            self.request_data.compact(0.1);
            self.deadlines.remove(&request_data.deadline_key);
            let _ = request_data.response_completion.send(result);
            return Some(request_data.span);
        }

        tracing::debug!("No in-flight request found for request_id = {request_id}.");

        // If the response completion was absent, then the request was already canceled.
        None
    }

    /// Completes all requests using the provided function.
    /// Returns Spans for all completes requests.
    pub fn complete_all_requests<'a>(
        &'a mut self,
        mut result: impl FnMut() -> Res + 'a,
    ) -> impl Iterator<Item = Span> + 'a {
        self.deadlines.clear();
        self.request_data.drain().map(move |(_, request_data)| {
            let _ = request_data.response_completion.send(result());
            request_data.span
        })
    }

    /// Cancels a request without completing (typically used when a request handle was dropped
    /// before the request completed).
    pub fn cancel_request(&mut self, request_id: u64) -> Option<(context::Context, Span)> {
        if let Some(request_data) = self.request_data.remove(&request_id) {
            self.request_data.compact(0.1);
            self.deadlines.remove(&request_data.deadline_key);
            Some((request_data.ctx, request_data.span))
        } else {
            None
        }
    }

    /// Yields a request that has expired, completing it with a TimedOut error.
    /// The caller should send cancellation messages for any yielded request ID.
    pub fn poll_expired(
        &mut self,
        cx: &mut Context,
        expired_error: impl Fn() -> Res,
    ) -> Poll<Option<u64>> {
        self.deadlines.poll_expired(cx).map(|expired| {
            let request_id = expired?.into_inner();
            if let Some(request_data) = self.request_data.remove(&request_id) {
                let _entered = request_data.span.enter();
                tracing::error!("DeadlineExceeded");
                self.request_data.compact(0.1);
                let _ = request_data.response_completion.send(expired_error());
            }
            Some(request_id)
        })
    }
}
