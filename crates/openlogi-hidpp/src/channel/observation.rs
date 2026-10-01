//! Optional observation of reports and request lifecycles at the point where
//! the channel owns correlation.

use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
};

use tracing::trace;

use super::MAX_RAW_REPORT_LENGTH;

#[cfg(test)]
mod tests;

/// An owned raw HID report delivered to a [`ChannelObserver`].
///
/// Reports are bounded to the largest report accepted by the channel. The
/// channel only constructs this copy when observation is enabled.
#[derive(Clone, PartialEq, Eq)]
pub struct ObservedReport {
    bytes: [u8; MAX_RAW_REPORT_LENGTH],
    len: usize,
}

impl ObservedReport {
    pub(super) fn from_bytes(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= MAX_RAW_REPORT_LENGTH);
        let mut owned = [0; MAX_RAW_REPORT_LENGTH];
        owned[..bytes.len()].copy_from_slice(bytes);
        Self {
            bytes: owned,
            len: bytes.len(),
        }
    }

    /// Returns the report bytes, including the report ID.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Returns the report length, including the report ID.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the report is empty.
    ///
    /// Channel-generated observations are never empty; this method completes
    /// the conventional collection API.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl AsRef<[u8]> for ObservedReport {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Debug for ObservedReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ObservedReport")
            .field(&self.as_bytes())
            .finish()
    }
}

/// The terminal outcome of a request that expected a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestOutcome {
    /// A matching incoming report reached the request waiter.
    Succeeded,
    /// The request's total write-and-response budget elapsed.
    TimedOut,
    /// The raw transport rejected the outgoing report.
    WriteFailed,
    /// The response sender disappeared without delivering a report.
    NoResponse,
    /// The caller dropped the in-flight request future.
    Cancelled,
}

/// A wire or request-lifecycle fact observed by a [`HidppChannel`](super::HidppChannel).
///
/// Request IDs are monotonically assigned within one channel lifetime. An
/// outgoing report with `request_id: None` is a fire-and-forget HID++ write or
/// a [`HidppChannel::write_raw_report`](super::HidppChannel::write_raw_report)
/// write. A parsed incoming report has an ID only when the channel matched it
/// to a pending request; `None` therefore identifies unsolicited or late
/// input.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChannelObservation {
    /// A post-normalization report submitted to the raw transport.
    OutgoingReport {
        /// The pending request this report belongs to, if it expects a response.
        request_id: Option<u64>,
        /// The exact bytes passed to the raw transport.
        report: ObservedReport,
    },
    /// A valid HID++ report read from the raw transport.
    IncomingReport {
        /// The request whose predicate matched this report, if any.
        request_id: Option<u64>,
        /// The exact bytes returned by the raw transport.
        report: ObservedReport,
    },
    /// Raw incoming bytes rejected by HID++ report parsing.
    MalformedIncomingReport {
        /// The exact bytes returned by the raw transport.
        report: ObservedReport,
    },
    /// The terminal outcome of a request that expected a response.
    RequestOutcome {
        /// The request whose lifecycle ended.
        request_id: u64,
        /// How the request ended.
        outcome: RequestOutcome,
    },
}

/// Receives optional channel observations.
///
/// The channel invokes observers synchronously and outside its internal locks,
/// after correlation decisions have been committed. Implementations must
/// return promptly and should hand events to their own worker if processing
/// may block. Observer panics are caught so they cannot stop the reader thread
/// or change channel shutdown.
///
/// Calls originate from request executors and the dedicated reader thread, so
/// concurrent requests can invoke an observer concurrently. A successful
/// request is woken before its matching incoming observation is delivered;
/// consumers must associate events by request ID rather than callback order.
pub trait ChannelObserver: Send + Sync + 'static {
    /// Receives one owned observation.
    fn observe(&self, observation: ChannelObservation);
}

impl<F> ChannelObserver for F
where
    F: Fn(ChannelObservation) + Send + Sync + 'static,
{
    fn observe(&self, observation: ChannelObservation) {
        self(observation);
    }
}

pub(super) fn emit(observer: &dyn ChannelObserver, observation: ChannelObservation) {
    if catch_unwind(AssertUnwindSafe(|| observer.observe(observation))).is_err() {
        trace!("channel observer panicked — observation ignored");
    }
}

pub(super) fn emit_report(
    observer: Option<&dyn ChannelObserver>,
    bytes: &[u8],
    observation: impl FnOnce(ObservedReport) -> ChannelObservation,
) {
    if let Some(observer) = observer {
        emit(observer, observation(ObservedReport::from_bytes(bytes)));
    }
}

pub(super) struct RequestObservation<'a> {
    observer: Option<&'a dyn ChannelObserver>,
    request_id: u64,
    completed: bool,
}

impl<'a> RequestObservation<'a> {
    pub(super) fn new(observer: Option<&'a dyn ChannelObserver>, request_id: u64) -> Self {
        Self {
            observer,
            request_id,
            completed: false,
        }
    }

    pub(super) fn complete(&mut self, outcome: RequestOutcome) {
        self.completed = true;
        if let Some(observer) = self.observer {
            emit(
                observer,
                ChannelObservation::RequestOutcome {
                    request_id: self.request_id,
                    outcome,
                },
            );
        }
    }
}

impl Drop for RequestObservation<'_> {
    fn drop(&mut self) {
        if !self.completed
            && let Some(observer) = self.observer
        {
            emit(
                observer,
                ChannelObservation::RequestOutcome {
                    request_id: self.request_id,
                    outcome: RequestOutcome::Cancelled,
                },
            );
        }
    }
}
