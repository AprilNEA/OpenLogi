//! Implements basic messaging across HID and HID++ channels.
//!
//! This includes mapping incoming messages to previously sent requests.

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
};

use async_trait::async_trait;
use futures::{FutureExt, channel::oneshot, select};
use hidreport::{Field, Report, ReportDescriptor, Usage, UsageId, UsagePage};
use rand::Rng;
use thiserror::Error;

use crate::nibble::U4;

/// hidapi defines this as the maximum EXPECTED size of report descriptors.
/// We will trust this for now, but a workaround may be required if devices do
/// in fact return longer descriptors.
const MAX_REPORT_DESCRIPTOR_LENGTH: usize = 4096;

/// This is the size of the buffer incoming reports are read into.
/// As we only care about HID++ reports, this equals to [`LONG_REPORT_LENGTH`].
const MAX_REPORT_LENGTH: usize = LONG_REPORT_LENGTH;

/// The ID of the HID report that is used to transmit short HID++ messages.
pub const SHORT_REPORT_ID: u8 = 0x10;

/// The HID usage page ID of short HID++ message reports.
pub const SHORT_REPORT_USAGE_PAGE: u16 = 0xff00;

/// The HID usage ID of short HID++ message reports.
pub const SHORT_REPORT_USAGE: u16 = 0x0001;

/// The length of short HID++ message reports (including report ID).
pub const SHORT_REPORT_LENGTH: usize = 7;

/// The ID of the HID report that is used to transmit long HID++ messages.
pub const LONG_REPORT_ID: u8 = 0x11;

/// The HID usage page ID of long HID++ message reports.
pub const LONG_REPORT_USAGE_PAGE: u16 = 0xff00;

/// The HID usage ID of long HID++ message reports.
pub const LONG_REPORT_USAGE: u16 = 0x0002;

/// The length of long HID++ message reports (including report ID).
pub const LONG_REPORT_LENGTH: usize = 20;

/// Represents an arbitrary HID communication channel that is both readable and
/// writable. It has to support async I/O.
///
/// Any type this trait is implemented for can be used for HID(++)
/// communication. If a specific channel supports HID++ is determined at a later
/// stage and is not directly related to potential implementations of this
/// trait.
#[async_trait]
pub trait RawHidChannel: Sync + Send + 'static {
    /// Provides the vendor ID of the connected HID device.
    fn vendor_id(&self) -> u16;

    /// Provides the product ID of the connected HID device.
    fn product_id(&self) -> u16;

    /// Writes a raw report to the channel.
    ///
    /// Returns the exact amount of written bytes on success.
    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Sync + Send>>;

    /// Reads a raw report from the channel.
    ///
    /// If the buffer is not large enough to fit the whole report, its remainder
    /// should be discarded and must not be returned by any succeeding call to
    /// [`Self::read_report`].
    ///
    /// Returns the exact amount or read bytes on success.
    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Sync + Send>>;

    /// If the implementation already knows whether the underlying HID channel
    /// supports HID++ messages, it should return `Some((supports_short,
    /// supports_long))` from this method.
    ///
    /// In this case, the report descriptor will not be read and parsed.
    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)>;

    /// Retrieves the raw HID report descriptor from the channel.
    ///
    /// This is used to determine whether the channel supports HID++.
    ///
    /// Returns the exact size of the report descriptor on success.
    async fn get_report_descriptor(
        &self,
        buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>>;
}

/// Checks whether a raw channel supports short or long HID++ messages.
async fn supports_short_long_hidpp(
    chan: &impl RawHidChannel,
) -> Result<(bool, bool), ChannelError> {
    if let Some((supports_short, supports_long)) = chan.supports_short_long_hidpp() {
        return Ok((supports_short, supports_long));
    }

    let mut raw_descriptor = vec![0u8; MAX_REPORT_DESCRIPTOR_LENGTH];
    let descriptor_size = chan.get_report_descriptor(&mut raw_descriptor).await?;

    let descriptor = match ReportDescriptor::try_from(&raw_descriptor[..descriptor_size]) {
        Ok(val) => val,
        Err(err) => return Err(ChannelError::ReportDescriptor(err)),
    };

    let supports_short = descriptor
        .find_input_report(&[SHORT_REPORT_ID])
        .and_then(|report| report.fields().first())
        .and_then(|field| match field {
            Field::Array(arr) => Some(arr.usage_range()),
            _ => None,
        })
        .is_some_and(|range| {
            range
                .lookup_usage(&Usage::from_page_and_id(
                    UsagePage::from(SHORT_REPORT_USAGE_PAGE),
                    UsageId::from(SHORT_REPORT_USAGE),
                ))
                .is_some()
        });

    let supports_long = descriptor
        .find_input_report(&[LONG_REPORT_ID])
        .and_then(|report| report.fields().first())
        .and_then(|field| match field {
            Field::Array(arr) => Some(arr.usage_range()),
            _ => None,
        })
        .is_some_and(|range| {
            range
                .lookup_usage(&Usage::from_page_and_id(
                    UsagePage::from(LONG_REPORT_USAGE_PAGE),
                    UsageId::from(LONG_REPORT_USAGE),
                ))
                .is_some()
        });

    Ok((supports_short, supports_long))
}

/// Represents an unversioned HID++ message.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HidppMessage {
    /// Represents a short HID++ message.
    ///
    /// Please check [`HidppChannel::supports_short`] before sending this kind
    /// of message.
    Short([u8; SHORT_REPORT_LENGTH - 1]),

    /// Represents a long HID++ message.
    ///
    /// Please check [`HidppChannel::supports_long`] before sending this kind of
    /// message.
    Long([u8; LONG_REPORT_LENGTH - 1]),
}

impl HidppMessage {
    /// Tries to read a HID++ message from raw data.
    pub fn read_raw(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        if data[0] == SHORT_REPORT_ID {
            if data.len() != SHORT_REPORT_LENGTH {
                return None;
            }

            return Some(HidppMessage::Short(data[1..].try_into().unwrap()));
        } else if data[0] == LONG_REPORT_ID {
            if data.len() != LONG_REPORT_LENGTH {
                return None;
            }

            return Some(HidppMessage::Long(data[1..].try_into().unwrap()));
        }

        None
    }

    /// Writes a HID++ message in its raw byte form into a buffer.
    ///
    /// Returns the amount of written bytes.
    pub fn write_raw(&self, buf: &mut [u8]) -> usize {
        match self {
            Self::Short(payload) => {
                buf[0] = SHORT_REPORT_ID;
                buf[1..SHORT_REPORT_LENGTH].copy_from_slice(payload);
                SHORT_REPORT_LENGTH
            }
            Self::Long(payload) => {
                buf[0] = LONG_REPORT_ID;
                buf[1..LONG_REPORT_LENGTH].copy_from_slice(payload);
                LONG_REPORT_LENGTH
            }
        }
    }
}

type MessageListener = Box<dyn Fn(HidppMessage, bool) + Send>;

/// Represents a HID communication channel supporting HID++.
pub struct HidppChannel {
    /// Whether the channel supports short (7 bytes) HID++ messages.
    pub supports_short: bool,

    /// Whether the channel supports long (20 bytes) HID++ messages.
    pub supports_long: bool,

    /// The vendor ID of the connected HID device.
    pub vendor_id: u16,

    // The product ID of the connected HID device.
    pub product_id: u16,

    /// The underlying raw HID channel.
    raw_channel: Arc<dyn RawHidChannel>,

    /// Whether to rotate the [`Self::software_id`].
    rotate_software_id: AtomicBool,

    /// The software ID to provide at the next call to [`Self::get_sw_id`].
    software_id: AtomicU8,

    /// All sent messages that are waiting for a response.
    pending_messages: Arc<Mutex<VecDeque<PendingMessage>>>,

    /// Registered listeners that will receive notifications about incoming
    /// messages.
    message_listeners: Arc<Mutex<HashMap<u32, MessageListener>>>,

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
            read_thread_hdl.join().unwrap();
        }
    }
}

/// Represents a message that was sent and is waiting for a response.
struct PendingMessage {
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
    /// If the given HID channel does not support HID++,
    /// [`ChannelError::HidppNotSupported`] will be returned.
    pub async fn from_raw_channel(raw: impl RawHidChannel) -> Result<Self, ChannelError> {
        let (supports_short, supports_long) = supports_short_long_hidpp(&raw).await?;

        if !supports_short && !supports_long {
            return Err(ChannelError::HidppNotSupported);
        }

        let raw_channel_rc = Arc::new(raw);
        let pending_messages_rc = Arc::new(Mutex::new(VecDeque::<PendingMessage>::new()));
        let message_listeners_rc = Arc::new(Mutex::new(HashMap::<u32, MessageListener>::new()));

        let (close_sender, mut close_receiver) = oneshot::channel::<()>();

        let read_thread_hdl = thread::spawn({
            let raw_channel = Arc::clone(&raw_channel_rc);
            let pending_messages = Arc::clone(&pending_messages_rc);
            let message_listeners = Arc::clone(&message_listeners_rc);

            move || {
                futures::executor::block_on(async {
                    let mut buf = [0u8; MAX_REPORT_LENGTH];

                    loop {
                        let res = select! {
                            _ = close_receiver => {
                                break;
                            },
                            res = raw_channel.read_report(&mut buf).fuse() => res
                        };

                        let Ok(len) = res else {
                            continue;
                        };

                        let Some(msg) = HidppMessage::read_raw(&buf[..len]) else {
                            continue;
                        };

                        let mut msgs = pending_messages.lock().unwrap();
                        let mut matched = false;
                        if let Some(pos) =
                            msgs.iter().position(|elem| (elem.response_predicate)(&msg))
                        {
                            let waiting = msgs.remove(pos).unwrap();
                            let _ = waiting.sender.send(msg);
                            matched = true;
                        }

                        for listener in message_listeners.lock().unwrap().values() {
                            listener(msg, matched);
                        }
                    }
                });
            }
        });

        Ok(Self {
            supports_short,
            supports_long,
            vendor_id: raw_channel_rc.vendor_id(),
            product_id: raw_channel_rc.product_id(),
            raw_channel: raw_channel_rc,
            rotate_software_id: AtomicBool::new(false),
            software_id: AtomicU8::new(0x01),
            pending_messages: pending_messages_rc,
            message_listeners: message_listeners_rc,
            read_thread_close: Some(close_sender),
            read_thread_hdl: Some(read_thread_hdl),
        })
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

    /// Provides a software ID that can be used to send a HID++ message across
    /// the channel.
    ///
    /// This method should be called separately for every message to send as it
    /// may rotate (as indicated by [`Self::set_rotating_sw_id`]).
    pub fn get_sw_id(&self) -> U4 {
        if self.rotate_software_id.load(Ordering::SeqCst) {
            U4::from_lo(
                self.software_id
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                        Some(if old & 0x0f == 0x0f {
                            0x01
                        } else {
                            old.wrapping_add(1)
                        })
                    })
                    .unwrap(),
            )
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
            HidppMessage::Short(payload) if !self.supports_short && self.supports_long => {
                HidppMessage::Long(short_payload_as_long(&payload))
            }
            other => other,
        }
    }

    /// Sends a HID++ message across the channel and waits for a response.
    ///
    /// If no response is expected/required, use [`Self::send_and_forget`].
    ///
    /// The future resolves to `Ok(None)` if no response was received.
    pub async fn send(
        &self,
        msg: HidppMessage,
        response_predicate: impl Fn(&HidppMessage) -> bool + Send + 'static,
    ) -> Result<HidppMessage, ChannelError> {
        let msg = self.normalize_outgoing(msg);
        if !self.supports_msg(&msg) {
            return Err(ChannelError::MessageTypeNotSupported);
        }

        let (sender, receiver) = oneshot::channel::<HidppMessage>();

        self.pending_messages
            .lock()
            .unwrap()
            .push_back(PendingMessage {
                response_predicate: Box::new(response_predicate),
                sender,
            });

        self.send_and_forget(msg).await?;

        receiver.await.map_err(|_| ChannelError::NoResponse)
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

    /// Registers a listener that will be called for every incoming message.
    ///
    /// Returns a handle that can be used to remove the listener using a call to
    /// [`Self::remove_msg_listener`].
    pub fn add_msg_listener(&self, listener: impl Fn(HidppMessage, bool) + Send + 'static) -> u32 {
        let mut listeners = self.message_listeners.lock().unwrap();

        let mut rng = rand::rng();
        let mut hdl = rng.random::<u32>();
        while listeners.contains_key(&hdl) {
            hdl = rng.random::<u32>();
        }

        listeners.insert(hdl, Box::new(listener));
        hdl
    }

    /// Removes a previously registered message listener.
    ///
    /// Returns whether a listener was found using the given handle.
    pub fn remove_msg_listener(&self, hdl: u32) -> bool {
        self.message_listeners
            .lock()
            .unwrap()
            .remove(&hdl)
            .is_some()
    }
}

/// Represents an error that occurred when creating or interacting with a HID or
/// HID++ communication channel.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChannelError {
    /// Indicates that the concrete implementation of [`RawHidChannel`] returned
    /// an error.
    #[error("the HID channel implementation returned an error")]
    Implementation(#[from] Box<dyn Error + Sync + Send>),

    /// Indicates that the HID report descriptor could not be parsed.
    #[error("the report descriptor could not be parsed")]
    ReportDescriptor(hidreport::ParserError),

    /// Indicates that the channel in question does not support HID++.
    #[error("the HID channel does not support HID++")]
    HidppNotSupported,

    /// Indicates that the HID++ channel does not support messages of the given
    /// type (short/long).
    #[error("the channel does not support the given HID++ message type")]
    MessageTypeNotSupported,

    /// Indicates that no response was received following a request.
    #[error("the device did not respond to the request")]
    NoResponse,
}

/// Widen a short HID++ payload (6 bytes) to a long one (19 bytes): the HID++
/// header bytes (device / feature / function|sw) sit at the same offsets in
/// both widths, so the only change is zero-padding the trailing payload. Used
/// to re-frame short messages as long on a long-only channel — see
/// [`HidppChannel::normalize_outgoing`]. (OpenLogi local addition.)
fn short_payload_as_long(payload: &[u8; SHORT_REPORT_LENGTH - 1]) -> [u8; LONG_REPORT_LENGTH - 1] {
    let mut long = [0u8; LONG_REPORT_LENGTH - 1];
    long[..payload.len()].copy_from_slice(payload);
    long
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_payload_widens_preserving_header_and_padding() {
        // [device, feature, function|sw, p0, p1, p2]
        let short = [0xff, 0x05, 0x1e, 0xaa, 0xbb, 0xcc];
        let long = short_payload_as_long(&short);
        assert_eq!(&long[..short.len()], &short[..]); // header + payload copied verbatim
        assert!(long[short.len()..].iter().all(|&b| b == 0)); // remainder zero-padded
        assert_eq!(long.len(), LONG_REPORT_LENGTH - 1);
    }
}
