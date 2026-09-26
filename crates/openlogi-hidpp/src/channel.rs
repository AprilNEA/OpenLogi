//! Implements basic messaging across HID and HID++ channels.
//!
//! This includes mapping incoming messages to previously sent requests. A
//! reply is matched to the oldest pending request whose predicate accepts it,
//! and every predicate keys on the report's first three bytes — device,
//! feature (or HID++1.0 sub id), function and software id (or register
//! address). Two requests sharing those bytes therefore get replies nothing on
//! the wire can tell apart, and a wireless receiver may answer them out of
//! order (a retransmitted radio packet completes after a later one). The
//! channel never lets that happen: a request whose key is already in flight
//! waits until that request is answered — or, when it timed out or was
//! cancelled unanswered, until its reply lands and is discarded or
//! [`STALE_REPLY_GRACE`] passes without one. That holds even for a request
//! that re-asks the abandoned one byte for byte — the same question does not
//! promise the same answer once a write has gone between them — unless the
//! caller says otherwise with [`AbandonedReply::AdoptIdentical`], which only
//! a query of immutable state may.

use std::{
    any::Any,
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use futures::{FutureExt, channel::oneshot, select};
use tracing::trace;

use crate::{nibble::U4, sync::lock};

mod error;
mod message;
mod observation;
mod pending;
mod raw;

#[cfg(test)]
pub(crate) mod mock;
#[cfg(test)]
mod tests;

pub use error::ChannelError;
pub use message::{
    HidppMessage, LONG_REPORT_ID, LONG_REPORT_LENGTH, SHORT_REPORT_ID, SHORT_REPORT_LENGTH,
};
pub use observation::{ChannelObservation, ChannelObserver, ObservedReport, RequestOutcome};
pub use raw::RawHidChannel;

use observation::{RequestObservation, emit_report};
use pending::{
    PendingMessage, PendingQueue, PendingRequest, PendingRequestCompletion, StaleKey,
    finish_request,
};
use raw::supports_short_long_hidpp;

/// This is the size of the buffer incoming reports are read into.
/// As we only care about HID++ reports, this equals to [`LONG_REPORT_LENGTH`].
const MAX_REPORT_LENGTH: usize = LONG_REPORT_LENGTH;

/// Largest output report accepted by [`HidppChannel::write_raw_report`].
/// Logitech's very-long HID++ lighting report (`0x12`) is 64 bytes.
const MAX_RAW_REPORT_LENGTH: usize = 64;

/// The default time budget for a [`HidppChannel::send`] request: the report
/// write plus the wait for a matching response. Callers that need a different
/// budget can use [`HidppChannel::send_with_timeout`].
pub const SEND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a request abandoned unanswered — timed out, or cancelled by an
/// outer deadline — keeps its reply header reserved. A reply that lands inside
/// this window is discarded; one that never comes frees the header when the
/// window closes. Without the reservation a late reply would answer the next
/// request with the same header, which nothing on the wire can distinguish.
///
/// One second covers the late replies seen in practice: a receiver answering a
/// register read a few hundred milliseconds after a tight probe budget gave up
/// on it. It is deliberately much shorter than [`SEND_RESPONSE_TIMEOUT`]: a
/// device that never answers costs the next same-header request one grace
/// window, not a whole timeout, and a reply later than the grace is as
/// unattributable as it always was.
///
/// The quarantine makes no exception of its own for a request that re-asks
/// the abandoned one byte for byte. The bytes say what was asked, not what
/// the answer is: a DPI read abandoned before its reply, a DPI write
/// acknowledged, and the read re-asked all fit inside one grace window, and
/// the first read's late reply would hand the re-ask the value from before
/// the write. A caller whose query cannot be changed by any write may opt in
/// with [`AbandonedReply::AdoptIdentical`].
pub const STALE_REPLY_GRACE: Duration = Duration::from_secs(1);

/// What a request does about a reply still owed to an abandoned request with
/// the same header — see [`STALE_REPLY_GRACE`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AbandonedReply {
    /// Wait for it to land and be discarded, or for the grace to pass. The
    /// default, and the only safe choice for a query whose answer a write
    /// could have changed since the abandoned ask.
    #[default]
    Quarantine,
    /// Take it as this request's own answer, provided the abandoned request
    /// was byte-identical; the request goes out at once and is answered by
    /// whichever reply comes first, the other being discarded on arrival.
    /// The caller vouches that nothing can have changed the answer between
    /// the two asks — a lookup in a device's feature table, which is fixed
    /// for the life of the connection. A re-ask then costs no grace wait,
    /// which is what keeps a feature walk over a lossy link moving.
    AdoptIdentical,
}

type MessageListener = Arc<dyn Fn(HidppMessage, bool) + Send + Sync + 'static>;

/// Removes a HID++ message listener when dropped.
pub struct MessageListenerGuard {
    message_listeners: Weak<Mutex<HashMap<u32, MessageListener>>>,
    hdl: u32,
}

impl Drop for MessageListenerGuard {
    fn drop(&mut self) {
        if let Some(message_listeners) = self.message_listeners.upgrade() {
            lock(&message_listeners).remove(&self.hdl);
        }
    }
}

/// A software id a request may carry: `1..=15`.
///
/// Id `0` is the wire's device-notification marker (event decoding treats
/// `software_id == 0` as "not a response"), so a request sent with it would
/// have its response indistinguishable from an event — made unrepresentable
/// here by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestSwId(U4);

impl RequestSwId {
    /// The id as a request software id, or `None` for the reserved id `0`.
    #[must_use]
    pub fn new(id: U4) -> Option<Self> {
        (id.to_lo() != 0).then_some(Self(id))
    }

    /// The nibble the wire carries.
    #[must_use]
    pub fn get(self) -> U4 {
        self.0
    }
}

/// How the channel assigns the software id each outgoing request carries.
///
/// One value instead of three cooperating fields (a rotate flag, the current
/// id, an optional lease): the triple admitted states the wire cannot mean —
/// rotation walking over ids other channels hold leases on, or a leased id
/// different from the id actually sent. Constructed whole, those states are
/// unrepresentable.
pub enum SwIdPolicy {
    /// Every request carries the same id.
    Fixed(RequestSwId),
    /// Walk `1..=15`, one id per request — eases mapping responses to
    /// requests for a single exclusive user of a node. The counter is this
    /// policy's own; the id `0` slot is skipped in the wrap.
    Rotating(AtomicU8),
    /// A fixed id held for the channel's lifetime, so concurrent opens of one
    /// HID node never share a correlation id. The `lease` is whatever the
    /// allocator hands out to back it — an entry in a table, an OS file lock —
    /// and is dropped with the policy, which is when the id goes back.
    /// (OpenLogi local addition.)
    Leased {
        /// The leased id every request carries.
        id: RequestSwId,
        /// A second leased id for a distinct in-process consumer sharing this
        /// channel (e.g. input capture reusing an inventory-owned channel),
        /// so its requests correlate independently of `id`'s. `None` when the
        /// allocator could not spare a second id — those requests fall back
        /// to `id` and queue behind it as before. (OpenLogi local addition.)
        secondary: Option<RequestSwId>,
        /// Owns the lease(s); dropping it returns the id(s) to the allocator.
        lease: Box<dyn Any + Send + Sync>,
    },
}

impl SwIdPolicy {
    /// A fresh rotation, starting at id `1`.
    #[must_use]
    pub fn rotating() -> Self {
        Self::Rotating(AtomicU8::new(0x01))
    }
}

impl Default for SwIdPolicy {
    /// Fixed id `1`, matching the protocol's conventional default.
    fn default() -> Self {
        Self::Fixed(RequestSwId(U4::from_lo(0x01)))
    }
}

/// Represents a HID communication channel supporting HID++.
pub struct HidppChannel {
    /// Whether the channel supports short (7 bytes) HID++ messages.
    pub supports_short: bool,

    /// Whether the channel supports long (20 bytes) HID++ messages.
    pub supports_long: bool,

    /// The vendor ID of the connected HID device.
    pub vendor_id: u16,

    /// The product ID of the connected HID device.
    pub product_id: u16,

    /// The underlying raw HID channel.
    raw_channel: Arc<dyn RawHidChannel>,

    /// Optional sink for reports and request lifecycle events.
    observer: Option<Arc<dyn ChannelObserver>>,

    /// The software-id policy for outgoing requests (see [`SwIdPolicy`]).
    ///
    /// This must remain after `raw_channel`: fields drop in declaration order,
    /// so the final lease is returned only after channel shutdown has joined
    /// the read thread and released the raw transport.
    sw_id_policy: SwIdPolicy,

    /// All sent messages that are waiting for a response, and the requests
    /// parked behind one that shares their header.
    pending_messages: Arc<Mutex<PendingQueue>>,

    /// The request ID assigned to the next pending message.
    pending_message_id: AtomicU64,

    /// Registered listeners that will receive notifications about incoming
    /// messages.
    message_listeners: Arc<Mutex<HashMap<u32, MessageListener>>>,

    /// The handle assigned to the next registered message listener.
    ///
    /// A counter, not a random draw: a handle is only ever a key into
    /// [`Self::message_listeners`], so counting up is collision-free by
    /// construction where drawing needed a retry loop to be merely unlikely.
    next_listener_hdl: AtomicU32,

    /// The sender signaling the read thread to stop.
    read_thread_close: Option<oneshot::Sender<()>>,

    /// The handle to the read thread. Should be joined after signaling
    /// [`Self::read_thread_close`].
    read_thread_hdl: Option<JoinHandle<()>>,
}

impl Drop for HidppChannel {
    fn drop(&mut self) {
        if let Some(read_thread_close) = self.read_thread_close.take() {
            // This only fails if the receiving end, which is owned by the read thread in
            // this case, is dropped.
            // This just means that the read thread is already stopped, so we can ignore the
            // error here.
            let _ = read_thread_close.send(());
        }

        if let Some(read_thread_hdl) = self.read_thread_hdl.take() {
            // Joining is not politeness: together with the subsequent
            // `raw_channel` field drop, it makes the OS handle close before the
            // software-id lease is returned. A caller can therefore drop a
            // channel and reopen the same node without overlapping lifetimes.
            #[expect(
                clippy::unwrap_used,
                reason = "propagate a read-thread panic instead of ignoring a crashed background worker"
            )]
            read_thread_hdl.join().unwrap();
        }
    }
}

impl HidppChannel {
    /// Tries to construct a HID++ channel from a raw HID channel.
    ///
    /// Reads run on a thread this owns, joined on drop — so the underlying OS
    /// handle is closed by the time a dropped channel's `drop` returns. Callers
    /// that reopen the same node right after closing one depend on that.
    ///
    /// If the given HID channel does not support HID++,
    /// [`ChannelError::HidppNotSupported`] will be returned.
    pub async fn from_raw_channel(raw: impl RawHidChannel) -> Result<Self, ChannelError> {
        Self::from_raw_channel_inner(raw, None).await
    }

    /// Tries to construct a HID++ channel that reports wire and request facts
    /// to `observer`.
    ///
    /// This has the same channel behavior and failure contract as
    /// [`Self::from_raw_channel`]. Observation is best enabled at construction
    /// so incoming reports cannot race observer installation.
    pub async fn from_raw_channel_with_observer(
        raw: impl RawHidChannel,
        observer: Arc<dyn ChannelObserver>,
    ) -> Result<Self, ChannelError> {
        Self::from_raw_channel_inner(raw, Some(observer)).await
    }

    async fn from_raw_channel_inner(
        raw: impl RawHidChannel,
        observer: Option<Arc<dyn ChannelObserver>>,
    ) -> Result<Self, ChannelError> {
        let (supports_short, supports_long) = supports_short_long_hidpp(&raw).await?;

        if !supports_short && !supports_long {
            return Err(ChannelError::HidppNotSupported);
        }

        let raw_channel_rc = Arc::new(raw);
        let pending_messages_rc = Arc::new(Mutex::new(PendingQueue::default()));
        let message_listeners_rc = Arc::new(Mutex::new(HashMap::<u32, MessageListener>::new()));

        let (close_sender, close_receiver) = oneshot::channel::<()>();

        let read_thread_hdl = thread::spawn({
            let raw_channel = Arc::clone(&raw_channel_rc);
            let pending_messages = Arc::clone(&pending_messages_rc);
            let message_listeners = Arc::clone(&message_listeners_rc);
            let observer = observer.clone();

            move || {
                futures::executor::block_on(read_loop(
                    &*raw_channel,
                    &pending_messages,
                    &message_listeners,
                    observer.as_deref(),
                    close_receiver,
                ));
            }
        });

        Ok(Self {
            supports_short,
            supports_long,
            vendor_id: raw_channel_rc.vendor_id(),
            product_id: raw_channel_rc.product_id(),
            raw_channel: raw_channel_rc,
            observer,
            sw_id_policy: SwIdPolicy::default(),
            pending_messages: pending_messages_rc,
            pending_message_id: AtomicU64::new(1),
            next_listener_hdl: AtomicU32::new(1),
            message_listeners: message_listeners_rc,
            read_thread_close: Some(close_sender),
            read_thread_hdl: Some(read_thread_hdl),
        })
    }

    /// Whether the underlying HID transport still reports a live connection.
    pub fn is_connected(&self) -> bool {
        self.raw_channel.is_connected()
    }

    /// Replace the software-id policy for outgoing requests.
    ///
    /// `&mut self` on purpose: the policy is decided while the channel is
    /// still exclusively owned (right after opening, before it is shared), so
    /// no request can race a policy change. Replacing a [`SwIdPolicy::Leased`]
    /// policy returns its lease before this method returns. The channel's final
    /// lease is returned only after its read thread and raw transport stop.
    pub fn set_sw_id_policy(&mut self, policy: SwIdPolicy) {
        self.sw_id_policy = policy;
    }

    /// Provides a software ID that can be used to send a HID++ message across
    /// the channel.
    ///
    /// This method should be called separately for every message to send, as a
    /// [`SwIdPolicy::Rotating`] policy advances per call.
    pub fn get_sw_id(&self) -> U4 {
        match &self.sw_id_policy {
            SwIdPolicy::Fixed(id) | SwIdPolicy::Leased { id, .. } => id.get(),
            SwIdPolicy::Rotating(counter) => {
                // The closure always returns `Some`, so `fetch_update` never
                // reports `Err`; both arms carry the same pre-update value.
                let previous =
                    match counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                        Some(if old & 0x0f == 0x0f {
                            0x01
                        } else {
                            old.wrapping_add(1)
                        })
                    }) {
                        Ok(previous) | Err(previous) => previous,
                    };
                U4::from_lo(previous)
            }
        }
    }

    /// The software id a second, distinct in-process consumer of this channel
    /// should use — e.g. input capture, when it reuses an inventory-owned
    /// channel per its own session lifetime, alongside inventory's own probe
    /// traffic. Falls back to [`Self::get_sw_id`] when the policy has no
    /// second id reserved, which is exactly the pre-existing single-consumer
    /// behavior: the two consumers then share a correlation key and queue
    /// behind each other, as they always did.
    pub fn get_secondary_sw_id(&self) -> U4 {
        match &self.sw_id_policy {
            SwIdPolicy::Leased {
                secondary: Some(id),
                ..
            } => id.get(),
            _ => self.get_sw_id(),
        }
    }

    /// Checks whether the channel supports the given HID++ message.
    pub fn supports_msg(&self, msg: &HidppMessage) -> bool {
        match msg {
            HidppMessage::Short(_) => self.supports_short,
            HidppMessage::Long(_) => self.supports_long,
        }
    }

    /// Re-frames a short message as long on a long-only channel — a device that
    /// exposes only the long HID++ report (e.g. a Bluetooth-LE-direct mouse on
    /// macOS, where `IOHIDDeviceSetReport` rejects the short report). The HID++
    /// header bytes sit at the same offsets in both widths, so the only change
    /// is the report id plus zero-padding the extra payload; the device answers
    /// with a long report, which still matches the request by header. A no-op on
    /// channels that advertise short support.
    ///
    /// (OpenLogi local addition — candidate for upstreaming.)
    fn normalize_outgoing(&self, msg: HidppMessage) -> HidppMessage {
        match msg {
            HidppMessage::Short(_) if !self.supports_short && self.supports_long => msg.widened(),
            other => other,
        }
    }

    /// Sends a HID++ message across the channel and waits for a response.
    ///
    /// If no response is expected/required, use [`Self::send_and_forget`].
    ///
    /// The whole request — the report write plus the wait for a matching
    /// response — is bounded by [`SEND_RESPONSE_TIMEOUT`]; the future resolves
    /// to [`ChannelError::Timeout`] on elapse. Use [`Self::send_with_timeout`]
    /// to choose a different budget.
    pub async fn send(
        &self,
        msg: HidppMessage,
        response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
    ) -> Result<HidppMessage, ChannelError> {
        self.send_with_timeout(msg, response_predicate, SEND_RESPONSE_TIMEOUT)
            .await
    }

    /// Sends a HID++ message across the channel and waits for a response,
    /// bounding the whole request — the report write plus the wait for a
    /// matching response — by `timeout`.
    ///
    /// On elapse the request's pending entry is removed (concurrent in-flight
    /// requests are unaffected) and [`ChannelError::Timeout`] is returned; a
    /// response that still arrives later reaches message listeners as an
    /// unmatched message.
    ///
    /// A request whose header — device, feature, function and software id —
    /// is already in flight goes out only once that request is answered:
    /// their replies would be indistinguishable, and a receiver may deliver
    /// them out of order. If that request timed out or was cancelled
    /// unanswered, its reply is still expected: the header stays reserved
    /// until the reply lands and is discarded, or for [`STALE_REPLY_GRACE`]
    /// at most — a byte-identical re-ask included, since a write may have gone
    /// between the two asks. The wait counts against `timeout`. A query of
    /// immutable state can adopt that reply instead through [`Self::send_with`].
    ///
    /// [`Self::send`] uses this with [`SEND_RESPONSE_TIMEOUT`], which suits
    /// requests to a device that may be asleep. Requests that should fail
    /// faster — e.g. probing a receiver that answers immediately or not at
    /// all — can pass a tighter budget.
    pub async fn send_with_timeout(
        &self,
        msg: HidppMessage,
        response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
        timeout: Duration,
    ) -> Result<HidppMessage, ChannelError> {
        self.send_with(msg, response_predicate, timeout, AbandonedReply::Quarantine)
            .await
    }

    /// [`Self::send_with_timeout`], choosing what to do about a reply still
    /// owed to an abandoned request with the same header. Only a query whose
    /// answer no write can change should pass
    /// [`AbandonedReply::AdoptIdentical`].
    pub async fn send_with(
        &self,
        msg: HidppMessage,
        response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
        timeout: Duration,
        abandoned: AbandonedReply,
    ) -> Result<HidppMessage, ChannelError> {
        let msg = self.normalize_outgoing(msg);
        if !self.supports_msg(&msg) {
            return Err(ChannelError::MessageTypeNotSupported);
        }

        // Wire trace (off by default; `OPENLOGI_LOG=hidpp=trace`). Capture the
        // header before `msg` is moved into the send future so the outcome line
        // below can name the same request.
        let (dev, feat, func) = msg.header();
        trace!(dev, feat, func, "hidpp request");

        let pending_id = self.pending_message_id.fetch_add(1, Ordering::SeqCst);
        let observation = RequestObservation::new(self.observer.as_deref(), pending_id);

        // The deadline covers the wait for an identical in-flight header and
        // the write as well: `write_report` has no bounded-time contract of its
        // own, so a wedged device could otherwise park `send` forever before
        // the response wait even starts.
        let completion = {
            let mut request = std::pin::pin!(
                async move {
                    let pending_request = PendingRequest::register_when_key_free(
                        pending_id,
                        Arc::clone(&self.pending_messages),
                        msg,
                        abandoned,
                        response_predicate,
                    )
                    .await;
                    if let Err(error) = self.write_hidpp_report(msg, Some(pending_id)).await {
                        return PendingRequestCompletion::WriteFailed(error);
                    }
                    pending_request.receive().await.map_or(
                        PendingRequestCompletion::NoResponse,
                        PendingRequestCompletion::Response,
                    )
                }
                .fuse()
            );

            select! {
                completion = request => completion,
                () = futures_timer::Delay::new(timeout).fuse() => PendingRequestCompletion::TimedOut,
            }
        };

        finish_request(completion, observation, dev, feat)
    }

    /// Sends a HID++ message without timing out its transport write, then
    /// waits at most `response_timeout` for a matching response.
    ///
    /// This ordering is for an owner that must know the native write has
    /// completed before issuing a later request such as rollback. A native HID
    /// implementation may keep a write alive after its Rust future is dropped,
    /// so timing out and discarding that future cannot guarantee wire order.
    ///
    /// The caller **must drive this future to completion**, even after its own
    /// requester has cancelled or exceeded a deadline. Requester deadlines
    /// belong outside the task that owns this future and should only signal
    /// that owner. A wedged native write cannot honestly be bounded without a
    /// transport-level cancellation or completion guarantee.
    ///
    /// Pending-request cleanup, observations, and response matching otherwise
    /// follow [`Self::send_with_timeout`].
    pub async fn send_write_through(
        &self,
        msg: HidppMessage,
        response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
        response_timeout: Duration,
    ) -> Result<HidppMessage, ChannelError> {
        let msg = self.normalize_outgoing(msg);
        if !self.supports_msg(&msg) {
            return Err(ChannelError::MessageTypeNotSupported);
        }

        let (dev, feat, func) = msg.header();
        trace!(dev, feat, func, "hidpp request");

        let pending_id = self.pending_message_id.fetch_add(1, Ordering::SeqCst);
        let observation = RequestObservation::new(self.observer.as_deref(), pending_id);
        let pending_request = PendingRequest::register_when_key_free(
            pending_id,
            Arc::clone(&self.pending_messages),
            msg,
            AbandonedReply::Quarantine,
            response_predicate,
        )
        .await;

        let completion = if let Err(error) = self.write_hidpp_report(msg, Some(pending_id)).await {
            drop(pending_request);
            PendingRequestCompletion::WriteFailed(error)
        } else {
            let mut response = std::pin::pin!(pending_request.receive().fuse());
            select! {
                response = response => response.map_or(
                    PendingRequestCompletion::NoResponse,
                    PendingRequestCompletion::Response,
                ),
                () = futures_timer::Delay::new(response_timeout).fuse() => {
                    PendingRequestCompletion::TimedOut
                },
            }
        };

        finish_request(completion, observation, dev, feat)
    }

    /// Sends a HID++ message across the channel and does not wait for a
    /// response.
    ///
    /// If a response is expected, use [`Self::send`],
    pub async fn send_and_forget(&self, msg: HidppMessage) -> Result<(), ChannelError> {
        let msg = self.normalize_outgoing(msg);
        if !self.supports_msg(&msg) {
            return Err(ChannelError::MessageTypeNotSupported);
        }

        self.write_hidpp_report(msg, None).await
    }

    async fn write_hidpp_report(
        &self,
        msg: HidppMessage,
        request_id: Option<u64>,
    ) -> Result<(), ChannelError> {
        let mut buf = [0u8; LONG_REPORT_LENGTH];
        let len = msg.write_raw(&mut buf);
        emit_report(self.observer.as_deref(), &buf[..len], |report| {
            ChannelObservation::OutgoingReport { request_id, report }
        });
        self.raw_channel
            .write_report(&buf[..len])
            .await
            .map(|_| ())
            .map_err(ChannelError::Implementation)
    }

    /// Write one raw HID report through this channel's already-owned transport.
    ///
    /// Reports must contain `1..=64` bytes, including their report ID. The
    /// operation is bounded by [`SEND_RESPONSE_TIMEOUT`] and returns the exact
    /// byte count reported by the transport. This is intended for HID++ report
    /// widths such as the 64-byte `0x12` lighting frame that [`HidppMessage`]
    /// cannot represent.
    pub async fn write_raw_report(&self, report: &[u8]) -> Result<usize, ChannelError> {
        self.write_raw_report_with_timeout(report, SEND_RESPONSE_TIMEOUT)
            .await
    }

    async fn write_raw_report_with_timeout(
        &self,
        report: &[u8],
        timeout: Duration,
    ) -> Result<usize, ChannelError> {
        if !(1..=MAX_RAW_REPORT_LENGTH).contains(&report.len()) {
            return Err(ChannelError::InvalidRawReportLength(report.len()));
        }

        emit_report(self.observer.as_deref(), report, |report| {
            ChannelObservation::OutgoingReport {
                request_id: None,
                report,
            }
        });
        let mut write = std::pin::pin!(self.raw_channel.write_report(report).fuse());
        select! {
            result = write => result.map_err(ChannelError::Implementation),
            () = futures_timer::Delay::new(timeout).fuse() => Err(ChannelError::Timeout),
        }
    }

    /// Registers a listener that will be called for every incoming message.
    ///
    /// Returns a handle that can be used to remove the listener using a call to
    /// [`Self::remove_msg_listener`].
    pub fn add_msg_listener(
        &self,
        listener: impl Fn(HidppMessage, bool) + Send + Sync + 'static,
    ) -> u32 {
        let hdl = self.next_listener_hdl.fetch_add(1, Ordering::Relaxed);
        lock(&self.message_listeners).insert(hdl, Arc::new(listener));
        hdl
    }

    /// Registers a listener that is automatically removed when the returned
    /// guard is dropped.
    pub fn add_msg_listener_guarded(
        &self,
        listener: impl Fn(HidppMessage, bool) + Send + Sync + 'static,
    ) -> MessageListenerGuard {
        let hdl = self.add_msg_listener(listener);
        MessageListenerGuard {
            message_listeners: Arc::downgrade(&self.message_listeners),
            hdl,
        }
    }

    /// Removes a previously registered message listener.
    ///
    /// Returns whether a listener was found using the given handle.
    pub fn remove_msg_listener(&self, hdl: u32) -> bool {
        lock(&self.message_listeners).remove(&hdl).is_some()
    }
}

/// Reads reports from `raw_channel` until `close` fires, resolving each one
/// against the pending requests and then handing it to every listener.
///
/// Runs on the channel's dedicated read thread. `read_report` is always raced
/// against `close` so a transport that parks forever on a dead device still
/// lets the channel shut down — see [`RawHidChannel::read_report`].
async fn read_loop(
    raw_channel: &dyn RawHidChannel,
    pending_messages: &Mutex<PendingQueue>,
    message_listeners: &Mutex<HashMap<u32, MessageListener>>,
    observer: Option<&dyn ChannelObserver>,
    mut close: oneshot::Receiver<()>,
) {
    let mut buf = [0u8; MAX_REPORT_LENGTH];

    loop {
        let res = select! {
            _ = close => break,
            res = raw_channel.read_report(&mut buf).fuse() => res,
        };

        let len = match res {
            Ok(len) => len,
            Err(error) => {
                // A silently erroring handle is indistinguishable from a deaf
                // one without this line.
                trace!(?error, "read_report error");
                continue;
            }
        };

        let Some(msg) = HidppMessage::read_raw(&buf[..len]) else {
            emit_report(observer, &buf[..len], |report| {
                ChannelObservation::MalformedIncomingReport { report }
            });
            trace!(len, "report not HID++ — dropped");
            continue;
        };

        let mut matched_id = None;
        let mut stale = false;
        let pending_count;
        {
            let mut queue = lock(pending_messages);
            pending_count = queue.messages.len();
            if let Some(answered) = queue
                .messages
                .iter()
                .position(|elem| (elem.response_predicate)(&msg))
                .and_then(|pos| queue.messages.remove(pos))
            {
                matched_id = Some(answered.id);
                let PendingMessage {
                    key,
                    request,
                    response_predicate,
                    sender,
                    extra_replies,
                    ..
                } = answered;
                let _ = sender.send(msg);
                if extra_replies > 0 {
                    // Answered by one of several replies it was owed: the
                    // others are still coming and must not answer the next
                    // request with this header.
                    queue.stale.push(StaleKey {
                        key,
                        request,
                        response_predicate,
                        outstanding: extra_replies,
                        expires: Instant::now() + STALE_REPLY_GRACE,
                    });
                }
                queue.wake_key_waiters();
            } else {
                // The reply of a request nobody waits for any more. Discarding
                // it here is what frees its header for the next request.
                stale = queue.discard_stale_reply(&msg);
            }
        }

        emit_report(observer, &buf[..len], |report| {
            ChannelObservation::IncomingReport {
                request_id: matched_id,
                report,
            }
        });

        trace!(
            len,
            matched = matched_id.is_some(),
            stale,
            pending_count,
            payload = format_args!("{:02x?}", &buf[..len.min(16)]),
            "raw report received"
        );

        // Collected before dispatch so a listener may add or remove listeners
        // without deadlocking on the lock it is being called under.
        let listeners: Vec<_> = lock(message_listeners).values().cloned().collect();
        for listener in listeners {
            listener(msg, matched_id.is_some());
        }
    }
}
