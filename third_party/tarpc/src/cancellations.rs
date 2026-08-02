use futures::{prelude::*, task::*};
use std::pin::Pin;
use tokio::sync::mpsc;

/// Sends request cancellation signals.
#[derive(Debug, Clone)]
pub struct RequestCancellation(mpsc::UnboundedSender<u64>);

/// A stream of IDs of requests that have been canceled.
#[derive(Debug)]
pub struct CanceledRequests(mpsc::UnboundedReceiver<u64>);

/// Returns a channel to send request cancellation messages.
pub fn cancellations() -> (RequestCancellation, CanceledRequests) {
    // Unbounded because messages are sent in the drop fn. This is fine, because it's still
    // bounded by the number of in-flight requests.
    let (tx, rx) = mpsc::unbounded_channel();
    (RequestCancellation(tx), CanceledRequests(rx))
}

impl RequestCancellation {
    /// Cancels the request with ID `request_id`.
    ///
    /// No validation is done of `request_id`. There is no way to know if the request id provided
    /// corresponds to a request actually tracked by the backing channel. `RequestCancellation` is
    /// a one-way communication channel.
    ///
    /// Once request data is cleaned up, a response will never be received by the client. This is
    /// useful primarily when request processing ends prematurely for requests with long deadlines
    /// which would otherwise continue to be tracked by the backing channel—a kind of leak.
    pub fn cancel(&self, request_id: u64) {
        let _ = self.0.send(request_id);
    }
}

impl CanceledRequests {
    /// Polls for a cancelled request.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<u64>> {
        self.0.poll_recv(cx)
    }
}

impl Stream for CanceledRequests {
    type Item = u64;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<u64>> {
        self.poll_recv(cx)
    }
}
