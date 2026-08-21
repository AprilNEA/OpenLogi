//! Implements basic messaging across HID and HID++ channels.
//!
//! This includes mapping incoming messages to previously sent requests.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use std::future::Future;

use futures::{FutureExt, channel::oneshot, select};
use tracing::trace;

use crate::{nibble::U4, sync::lock};

mod error;
mod message;
mod raw;

#[cfg(test)]
pub(crate) mod tests;

pub use error::ChannelError;
pub use message::{
    HidppMessage, LONG_REPORT_ID, LONG_REPORT_LENGTH, SHORT_REPORT_ID, SHORT_REPORT_LENGTH,
};
pub use raw::RawHidChannel;

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

    /// Whether to rotate the [`Self::software_id`].
    rotate_software_id: AtomicBool,

    /// The software ID to provide at the next call to [`Self::get_sw_id`].
    software_id: AtomicU8,

    /// All sent messages that are waiting for a response.
    pending_messages: Arc<Mutex<VecDeque<PendingMessage>>>,

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

    /// The sender that tells this channel's reader to stop.
    read_close: Option<oneshot::Sender<()>>,

    /// Optional process-wide software-id lease: `(id, free)` run on drop.
    ///
    /// OpenLogi leases a unique HID++ software id per open so concurrent
    /// channels on the same physical HID node never share a correlation id
    /// (software id `0` is reserved for device notifications). Local addition.
    sw_id_lease: Option<(u8, fn(u8))>,
}

impl Drop for HidppChannel {
    fn drop(&mut self) {
        if let Some((id, free)) = self.sw_id_lease.take() {
            free(id);
        }

        if let Some(read_close) = self.read_close.take() {
            // Fails only if the receiver is already gone, i.e. the reader has
            // already stopped — which is the state this is asking for.
            let _ = read_close.send(());
        }
    }
}

/// Represents a message that was sent and is waiting for a response.
struct PendingMessage {
    /// Unique ID used to remove this request if it times out.
    id: u64,

    /// The predicate that has to match for an incoming message to be classified
    /// as the response.
    response_predicate: Box<dyn Fn(&HidppMessage) -> bool + Send>,

    /// The oneshot sender used to provide the response message to the receiving
    /// end.
    sender: oneshot::Sender<HidppMessage>,
}

impl HidppChannel {
    /// Tries to construct a HID++ channel from a raw HID channel.
    ///
    /// Returns the channel and the future that drives its reads. **The reader
    /// must be polled** — spawned on the caller's executor, or raced against
    /// the caller's own work — or no response and no notification ever
    /// arrives, and every [`Self::send`] times out.
    ///
    /// Handing it back rather than starting it here is what keeps this crate
    /// free of an executor choice: it used to spawn an OS thread per channel
    /// and block on it, which cost a thread per open and cannot exist on a
    /// single-threaded target at all. Dropping the channel signals the reader
    /// to stop; it winds down on its executor rather than being joined.
    ///
    /// If the given HID channel does not support HID++,
    /// [`ChannelError::HidppNotSupported`] will be returned.
    pub async fn from_raw_channel(
        raw: impl RawHidChannel,
    ) -> Result<(Self, impl Future<Output = ()> + Send + 'static), ChannelError> {
        let (supports_short, supports_long) = supports_short_long_hidpp(&raw).await?;

        if !supports_short && !supports_long {
            return Err(ChannelError::HidppNotSupported);
        }

        let raw_channel_rc = Arc::new(raw);
        let pending_messages_rc = Arc::new(Mutex::new(VecDeque::<PendingMessage>::new()));
        let message_listeners_rc = Arc::new(Mutex::new(HashMap::<u32, MessageListener>::new()));

        let (close_sender, close_receiver) = oneshot::channel::<()>();

        let reader = {
            let raw_channel = Arc::clone(&raw_channel_rc);
            let pending_messages = Arc::clone(&pending_messages_rc);
            let message_listeners = Arc::clone(&message_listeners_rc);

            async move {
                read_loop(
                    &*raw_channel,
                    &pending_messages,
                    &message_listeners,
                    close_receiver,
                )
                .await;
            }
        };

        let channel = Self {
            supports_short,
            supports_long,
            vendor_id: raw_channel_rc.vendor_id(),
            product_id: raw_channel_rc.product_id(),
            raw_channel: raw_channel_rc,
            rotate_software_id: AtomicBool::new(false),
            software_id: AtomicU8::new(0x01),
            pending_messages: pending_messages_rc,
            pending_message_id: AtomicU64::new(1),
            next_listener_hdl: AtomicU32::new(1),
            message_listeners: message_listeners_rc,
            read_close: Some(close_sender),
            sw_id_lease: None,
        };
        Ok((channel, reader))
    }

    /// Whether the underlying HID transport still reports a live connection.
    pub fn is_connected(&self) -> bool {
        self.raw_channel.is_connected()
    }

    /// Sets the software ID that should be returned by the next call to
    /// [`Self::get_sw_id`].
    ///
    /// Using software ID `0` is highly discouraged as it is used for device
    /// notifications.
    pub fn set_sw_id(&self, sw_id: U4) {
        self.software_id.store(sw_id.to_lo(), Ordering::SeqCst);
    }

    /// Sets whether the software ID returned by a call to [`Self::get_sw_id`]
    /// should increment (and potentially wrap around) after each call.
    ///
    /// This comes in handy when trying to map responses to requests
    /// consistently.
    ///
    /// Software ID `0` will be skipped in the rotation process as it is
    /// reserved for device notifications.
    pub fn set_rotating_sw_id(&self, enable: bool) {
        self.rotate_software_id.store(enable, Ordering::SeqCst);
    }

    /// Lease software id `id` until this channel is dropped, then call `free(id)`.
    ///
    /// Replaces any previous lease. Used by OpenLogi so concurrent opens of the
    /// same HID node hold distinct correlation ids for their full lifetime.
    ///
    /// OpenLogi local addition.
    pub fn set_sw_id_lease(&mut self, id: u8, free: fn(u8)) {
        self.sw_id_lease = Some((id, free));
    }

    /// Provides a software ID that can be used to send a HID++ message across
    /// the channel.
    ///
    /// This method should be called separately for every message to send as it
    /// may rotate (as indicated by [`Self::set_rotating_sw_id`]).
    pub fn get_sw_id(&self) -> U4 {
        if self.rotate_software_id.load(Ordering::SeqCst) {
            // The closure always returns `Some`, so `fetch_update` never
            // reports `Err`; both arms carry the same pre-update value.
            let previous =
                match self
                    .software_id
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                        Some(if old & 0x0f == 0x0f {
                            0x01
                        } else {
                            old.wrapping_add(1)
                        })
                    }) {
                    Ok(previous) | Err(previous) => previous,
                };
            U4::from_lo(previous)
        } else {
            U4::from_lo(self.software_id.load(Ordering::SeqCst))
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
        let msg = self.normalize_outgoing(msg);
        if !self.supports_msg(&msg) {
            return Err(ChannelError::MessageTypeNotSupported);
        }

        // Wire trace (off by default; `OPENLOGI_LOG=hidpp=trace`). Capture the
        // header before `msg` is moved into the send future so the outcome line
        // below can name the same request.
        let (dev, feat, func) = msg.header();
        trace!(dev, feat, func, "hidpp request");

        let (sender, receiver) = oneshot::channel::<HidppMessage>();
        let pending_id = self.pending_message_id.fetch_add(1, Ordering::SeqCst);

        {
            let mut pending = lock(&self.pending_messages);
            // Drop abandoned requests before queuing this one. Timeouts and
            // write failures remove their entry eagerly below, but a caller
            // cancelled mid-flight (an outer `timeout(..)` dropping the whole
            // future) still leaves its `PendingMessage` behind. On a channel
            // reused across inventory ticks those would accumulate unboundedly
            // — and a late response could be mis-delivered to a recycled
            // software id. `is_canceled()` is true once the receiver is gone,
            // so this prunes exactly the give-ups.
            pending.retain(|m| !m.sender.is_canceled());
            pending.push_back(PendingMessage {
                id: pending_id,
                response_predicate: Box::new(response_predicate),
                sender,
            });
        }

        // The deadline covers the write as well: `write_report` has no
        // bounded-time contract of its own, so a wedged device could otherwise
        // park `send` forever before the response wait even starts.
        let mut request = std::pin::pin!(
            async {
                self.send_and_forget(msg).await?;
                receiver.await.map_err(|_| ChannelError::NoResponse)
            }
            .fuse()
        );

        let result = select! {
            result = request => result,
            () = futures_timer::Delay::new(timeout).fuse() => Err(ChannelError::Timeout),
        };

        match &result {
            Ok(_) => trace!(dev, feat, "hidpp response"),
            Err(e) => trace!(dev, feat, error = ?e, "hidpp no response"),
        }

        if result.is_err() {
            // A timeout or write failure leaves the entry queued — remove it
            // eagerly. After a matched response the reader has already
            // taken it, so this is a no-op then.
            self.remove_pending_message(pending_id);
        }

        result
    }

    fn remove_pending_message(&self, id: u64) {
        let mut pending = lock(&self.pending_messages);
        if let Some(pos) = pending.iter().position(|msg| msg.id == id) {
            pending.remove(pos);
        }
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

        let mut buf = [0u8; LONG_REPORT_LENGTH];
        let len = msg.write_raw(&mut buf);
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
/// Runs as the channel's reader future. `read_report` is always raced
/// against `close` so a transport that parks forever on a dead device still
/// lets the channel shut down — see [`RawHidChannel::read_report`].
async fn read_loop(
    raw_channel: &dyn RawHidChannel,
    pending_messages: &Mutex<VecDeque<PendingMessage>>,
    message_listeners: &Mutex<HashMap<u32, MessageListener>>,
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
            trace!(len, "report not HID++ — dropped");
            continue;
        };

        let mut matched = false;
        let pending_count;
        {
            let mut msgs = lock(pending_messages);
            pending_count = msgs.len();
            if let Some(pos) = msgs.iter().position(|elem| (elem.response_predicate)(&msg))
                && let Some(waiting) = msgs.remove(pos)
            {
                let _ = waiting.sender.send(msg);
                matched = true;
            }
        }

        trace!(
            len,
            matched,
            pending_count,
            payload = format_args!("{:02x?}", &buf[..len.min(16)]),
            "raw report received"
        );

        // Collected before dispatch so a listener may add or remove listeners
        // without deadlocking on the lock it is being called under.
        let listeners: Vec<_> = lock(message_listeners).values().cloned().collect();
        for listener in listeners {
            listener(msg, matched);
        }
    }
}
