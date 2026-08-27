//! Tests for the HID++ channel, and the mock transport they run on.
//!
//! `MockRawHidChannel` is also used by `device.rs`, so this module is
//! `pub(crate)` rather than private.

use super::*;
use std::{
    error::Error,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;

use crate::{
    nibble,
    protocol::v20::{self, ErrorType, Hidpp20Error},
};

static RELEASED_SW_IDS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ORDERING_RAW_CHANNEL_DROPPED: AtomicBool = AtomicBool::new(false);
static ORDERING_RELEASE_AFTER_RAW_DROP: AtomicBool = AtomicBool::new(false);
static ORDERING_RELEASE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// A live channel over the mock transport.
pub(crate) async fn channel_with_reader(raw: MockRawHidChannel) -> HidppChannel {
    HidppChannel::from_raw_channel(raw)
        .await
        .expect("the mock transport speaks HID++")
}

#[test]
fn replacing_and_dropping_leased_policies_releases_each_exactly_once() {
    futures::executor::block_on(async {
        RELEASED_SW_IDS.lock().unwrap().clear();
        let (raw, _handle) = MockRawHidChannel::new();
        let mut channel = channel_with_reader(raw).await;

        channel.set_sw_id_policy(leased_policy(1, record_sw_id_release));
        channel.set_sw_id_policy(leased_policy(2, record_sw_id_release));

        assert_eq!(*RELEASED_SW_IDS.lock().unwrap(), [1]);

        drop(channel);

        assert_eq!(*RELEASED_SW_IDS.lock().unwrap(), [1, 2]);
    });
}

#[test]
fn final_lease_releases_after_read_thread_and_raw_channel_stop() {
    futures::executor::block_on(async {
        ORDERING_RAW_CHANNEL_DROPPED.store(false, Ordering::SeqCst);
        ORDERING_RELEASE_AFTER_RAW_DROP.store(false, Ordering::SeqCst);
        ORDERING_RELEASE_COUNT.store(0, Ordering::SeqCst);
        let (raw, _handle) = MockRawHidChannel::with_drop_flag(Some(&ORDERING_RAW_CHANNEL_DROPPED));
        let mut channel = channel_with_reader(raw).await;
        channel.set_sw_id_policy(leased_policy(3, record_ordered_sw_id_release));

        drop(channel);

        assert!(ORDERING_RAW_CHANNEL_DROPPED.load(Ordering::SeqCst));
        assert!(ORDERING_RELEASE_AFTER_RAW_DROP.load(Ordering::SeqCst));
        assert_eq!(ORDERING_RELEASE_COUNT.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn short_payload_widens_preserving_header_and_padding() {
    // [device, feature, function|sw, p0, p1, p2]
    let short = [0xff, 0x05, 0x1e, 0xaa, 0xbb, 0xcc];
    let HidppMessage::Long(long) = HidppMessage::Short(short).widened() else {
        panic!("widening a short message must produce a long one");
    };
    assert_eq!(&long[..short.len()], &short[..]); // header + payload copied verbatim
    assert!(long[short.len()..].iter().all(|&b| b == 0)); // remainder zero-padded
    assert_eq!(long.len(), LONG_REPORT_LENGTH - 1);
}

#[test]
fn widening_an_already_long_message_is_a_no_op() {
    let long = HidppMessage::Long([0x5a; LONG_REPORT_LENGTH - 1]);

    assert_eq!(long.widened(), long);
}

#[test]
fn send_returns_response_before_timeout() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let request = short_msg(0x10);
        let response = short_msg(0x20);
        handle.queue_response(response);

        let actual = channel
            .send_with_timeout(
                request,
                move |candidate| *candidate == response,
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert_eq!(actual, response);
        assert_eq!(handle.written_reports().len(), 1);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_times_out_and_removes_pending_message() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let request = short_msg(0x10);
        let response = short_msg(0x20);

        let started = Instant::now();
        let err = channel
            .send_with_timeout(
                request,
                move |candidate| *candidate == response,
                Duration::from_millis(25),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ChannelError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(handle.written_reports().len(), 1);
        assert_pending_empty(&channel);
    });
}

#[test]
fn cancelled_send_removes_pending_before_a_late_response() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let late_response = short_msg(0x20);
        let mut send = Box::pin(channel.send_with_timeout(
            short_msg(0x10),
            move |candidate| *candidate == late_response,
            Duration::from_secs(1),
        ));

        assert!(futures::poll!(send.as_mut()).is_pending());
        assert_eq!(channel.pending_messages.lock().unwrap().len(), 1);

        drop(send);
        assert_pending_empty(&channel);

        handle.send_incoming(late_response).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (late_response, false));
    });
}

#[test]
fn timeout_removes_only_its_own_pending_message() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let never_answered = short_msg(0x20);
        let slow_response = short_msg(0x21);

        let timed_out = channel.send_with_timeout(
            short_msg(0x10),
            move |candidate| *candidate == never_answered,
            Duration::from_millis(25),
        );
        let answered = channel.send_with_timeout(
            short_msg(0x11),
            move |candidate| *candidate == slow_response,
            Duration::from_secs(1),
        );
        // Answer the second request only after the first has timed out, so
        // a removal that took the wrong entry would fail this test.
        let respond_late = async {
            futures_timer::Delay::new(Duration::from_millis(100)).await;
            handle.send_incoming(slow_response).await;
        };

        let (timed_out, answered, ()) = futures::join!(timed_out, answered, respond_late);

        assert!(matches!(timed_out.unwrap_err(), ChannelError::Timeout));
        assert_eq!(answered.unwrap(), slow_response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn late_response_after_timeout_is_ignored() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let request = short_msg(0x10);
        let late_response = short_msg(0x20);
        let err = channel
            .send_with_timeout(
                request,
                move |candidate| *candidate == late_response,
                Duration::from_millis(25),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ChannelError::Timeout));
        assert_pending_empty(&channel);

        handle.send_incoming(late_response).await;
        wait_for_event_count(&events, 1).await;
        assert_eq!(events.lock().unwrap()[0], (late_response, false));
        assert_pending_empty(&channel);

        let followup_request = short_msg(0x30);
        let followup_response = short_msg(0x40);
        handle.queue_response(followup_response);
        let actual = channel
            .send_with_timeout(
                followup_request,
                move |candidate| *candidate == followup_response,
                Duration::from_secs(1),
            )
            .await
            .unwrap();

        assert_eq!(actual, followup_response);
        wait_for_event_count(&events, 2).await;
        assert_eq!(events.lock().unwrap()[1], (followup_response, true));
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_and_forget_writes_without_pending_message() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        channel.send_and_forget(short_msg(0x10)).await.unwrap();

        assert_eq!(handle.written_reports().len(), 1);
        assert_pending_empty(&channel);
    });
}

#[test]
fn raw_report_write_forwards_exact_bytes_and_length() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let report = [0x12; MAX_RAW_REPORT_LENGTH];

        let written = channel.write_raw_report(&report).await.unwrap();

        assert_eq!(written, report.len());
        assert_eq!(handle.written_reports(), [report.to_vec()]);
    });
}

#[test]
fn raw_report_write_rejects_empty_and_oversized_inputs_without_io() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let empty = channel.write_raw_report(&[]).await.unwrap_err();
        let oversized = channel
            .write_raw_report(&[0; MAX_RAW_REPORT_LENGTH + 1])
            .await
            .unwrap_err();

        assert!(matches!(empty, ChannelError::InvalidRawReportLength(0)));
        assert!(matches!(
            oversized,
            ChannelError::InvalidRawReportLength(65)
        ));
        assert!(handle.written_reports().is_empty());
    });
}

#[test]
fn raw_report_write_times_out_when_the_transport_parks() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        handle.park_writes();
        let channel = channel_with_reader(raw).await;
        let started = Instant::now();

        let error = channel
            .write_raw_report_with_timeout(&[LONG_REPORT_ID], Duration::from_millis(25))
            .await
            .unwrap_err();

        assert!(matches!(error, ChannelError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(1));
    });
}

#[test]
fn listener_can_remove_another_listener_during_dispatch() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = Arc::new(channel_with_reader(raw).await);
        let removed_listener_calls = Arc::new(AtomicUsize::new(0));
        let removing_listener_calls = Arc::new(AtomicUsize::new(0));

        let removed_listener_calls_for_listener = Arc::clone(&removed_listener_calls);
        let removed_hdl = channel.add_msg_listener(move |_, _| {
            removed_listener_calls_for_listener.fetch_add(1, Ordering::SeqCst);
        });

        let channel_for_listener = Arc::clone(&channel);
        let removing_listener_calls_for_listener = Arc::clone(&removing_listener_calls);
        channel.add_msg_listener(move |_, _| {
            removing_listener_calls_for_listener.fetch_add(1, Ordering::SeqCst);
            channel_for_listener.remove_msg_listener(removed_hdl);
        });

        handle.send_incoming(short_msg(0x20)).await;
        wait_for_atomic_count(&removing_listener_calls, 1).await;
        wait_for_atomic_count(&removed_listener_calls, 1).await;

        handle.send_incoming(short_msg(0x21)).await;
        wait_for_atomic_count(&removing_listener_calls, 2).await;

        assert_eq!(removed_listener_calls.load(Ordering::SeqCst), 1);
    });
}

// --- HID++2.0 (v20) send/matcher characterization tests -----------------
//
// `HidppChannel::send`/`send_with_timeout` above are protocol-agnostic:
// they match on an arbitrary predicate over raw `HidppMessage`s. The
// v20-specific correlation logic (matching by header, splitting out error
// frames) lives in `protocol::v20::HidppChannel::send_v20`, which is built
// directly on top of `send`. These tests pin that logic's current
// behaviour using the same mock transport as the tests above.

#[test]
fn send_v20_matches_response_by_header_ignoring_unrelated_messages() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0x00, 0x00, 0x00]);
        let response = v20::Message::Short(header, [0xaa, 0xbb, 0xcc]);

        // Each decoy differs from the request in exactly one header field, so
        // none of them may be mistaken for its response.
        let wrong_device = v20::Message::Short(
            v20::MessageHeader {
                device_index: 0x02,
                ..header
            },
            [0, 0, 0],
        );
        let wrong_feature = v20::Message::Short(
            v20::MessageHeader {
                feature_index: 0x06,
                ..header
            },
            [0, 0, 0],
        );
        let wrong_sw_id = v20::Message::Short(
            v20::MessageHeader {
                software_id: U4::from_lo(0x4),
                ..header
            },
            [0, 0, 0],
        );

        let send_fut = channel.send_v20(request);
        let feed_fut = async {
            handle.send_incoming(wrong_device.into()).await;
            handle.send_incoming(wrong_feature.into()).await;
            handle.send_incoming(wrong_sw_id.into()).await;
            handle.send_incoming(response.into()).await;
        };

        let (result, ()) = futures::join!(send_fut, feed_fut);

        assert_eq!(result.unwrap(), response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_broadcast_event_does_not_resolve_pending_request() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        channel.add_msg_listener(move |msg, matched| {
            listener_events.lock().unwrap().push((msg, matched));
        });

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        let response = v20::Message::Short(header, [0xaa, 0xbb, 0xcc]);

        // Software ID 0 is reserved for unsolicited device notifications
        // (see `feature::event_payload`). The request above uses a non-zero
        // ID, so an incoming broadcast sharing device/feature but using ID 0
        // must be routed to listeners, not consumed as this request's
        // response.
        let event = v20::Message::Short(
            v20::MessageHeader {
                software_id: U4::from_lo(0x0),
                ..header
            },
            [0x01, 0x02, 0x03],
        );

        let send_fut = channel.send_v20(request);
        let feed_fut = async {
            handle.send_incoming(event.into()).await;
            wait_for_event_count(&events, 1).await;
            handle.send_incoming(response.into()).await;
        };

        let (result, ()) = futures::join!(send_fut, feed_fut);

        assert_eq!(result.unwrap(), response);
        // The oneshot resolves before the listener loop runs on the read
        // thread; wait for both deliveries before asserting on them.
        wait_for_event_count(&events, 2).await;
        let recorded = events.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                (HidppMessage::from(event), false),
                (HidppMessage::from(response), true),
            ]
        );
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_response_may_arrive_as_a_different_report_width() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        // Quirk: `send_v20`'s response predicate compares only the parsed
        // v20 header, not the underlying report width. A device replying
        // with a long report to a short request — same header, wider
        // payload — is still accepted as the response.
        let response = v20::Message::Long(header, [0xaa; 16]);
        handle.queue_response(response.into());

        let result = channel.send_v20(request).await.unwrap();

        assert_eq!(result, response);
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_error_frame_resolves_to_feature_error() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        let error_response = v20_error_frame(header, ErrorType::InvalidArgument.into());
        handle.queue_response(error_response.into());

        let err = channel.send_v20(request).await.unwrap_err();

        assert!(matches!(
            err,
            Hidpp20Error::Feature(ErrorType::InvalidArgument)
        ));
        assert_pending_empty(&channel);
    });
}

#[test]
fn send_v20_error_frame_with_unmapped_code_is_unsupported_response() {
    futures::executor::block_on(async {
        let (raw, handle) = MockRawHidChannel::new();
        let channel = channel_with_reader(raw).await;

        let header = v20::MessageHeader {
            device_index: 0x01,
            feature_index: 0x05,
            function_id: U4::from_lo(0x2),
            software_id: U4::from_lo(0x3),
        };
        let request = v20::Message::Short(header, [0, 0, 0]);
        // 0xfe is not a defined `ErrorType` variant.
        let error_response = v20_error_frame(header, 0xfe);
        handle.queue_response(error_response.into());

        let err = channel.send_v20(request).await.unwrap_err();

        assert!(matches!(err, Hidpp20Error::UnsupportedResponse));
        assert_pending_empty(&channel);
    });
}

/// Builds the HID++2.0 error-frame encoding for `request_header`: feature
/// index 0xFF, with the original feature index and function|software byte
/// shifted one byte to the right (see `v20::HidppChannel::send_v20`'s
/// `is_error` predicate for the reverse mapping).
fn v20_error_frame(request_header: v20::MessageHeader, error_code: u8) -> v20::Message {
    let error_header = v20::MessageHeader {
        device_index: request_header.device_index,
        feature_index: 0xff,
        function_id: U4::from_hi(request_header.feature_index),
        software_id: U4::from_lo(request_header.feature_index),
    };
    let mut payload = [0u8; 3];
    payload[0] = nibble::combine(request_header.function_id, request_header.software_id);
    payload[1] = error_code;
    v20::Message::Short(error_header, payload)
}

#[derive(Clone)]
pub(crate) struct MockRawHidHandle {
    incoming_tx: async_channel::Sender<Vec<u8>>,
    written_reports: Arc<Mutex<Vec<Vec<u8>>>>,
    responses_on_write: Arc<Mutex<VecDeque<Vec<u8>>>>,
    park_writes: Arc<AtomicBool>,
}

impl MockRawHidHandle {
    pub(crate) fn queue_response(&self, msg: HidppMessage) {
        self.responses_on_write
            .lock()
            .unwrap()
            .push_back(raw_report(msg));
    }

    async fn send_incoming(&self, msg: HidppMessage) {
        self.incoming_tx.send(raw_report(msg)).await.unwrap();
    }

    pub(crate) fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written_reports.lock().unwrap().clone()
    }

    fn park_writes(&self) {
        self.park_writes.store(true, Ordering::SeqCst);
    }
}

pub(crate) struct MockRawHidChannel {
    incoming_tx: async_channel::Sender<Vec<u8>>,
    incoming_rx: async_channel::Receiver<Vec<u8>>,
    written_reports: Arc<Mutex<Vec<Vec<u8>>>>,
    responses_on_write: Arc<Mutex<VecDeque<Vec<u8>>>>,
    park_writes: Arc<AtomicBool>,
    drop_flag: Option<&'static AtomicBool>,
}

impl MockRawHidChannel {
    pub(crate) fn new() -> (Self, MockRawHidHandle) {
        Self::with_drop_flag(None)
    }

    fn with_drop_flag(drop_flag: Option<&'static AtomicBool>) -> (Self, MockRawHidHandle) {
        let (incoming_tx, incoming_rx) = async_channel::unbounded();
        let written_reports = Arc::new(Mutex::new(Vec::new()));
        let responses_on_write = Arc::new(Mutex::new(VecDeque::new()));
        let park_writes = Arc::new(AtomicBool::new(false));

        let handle = MockRawHidHandle {
            incoming_tx: incoming_tx.clone(),
            written_reports: Arc::clone(&written_reports),
            responses_on_write: Arc::clone(&responses_on_write),
            park_writes: Arc::clone(&park_writes),
        };

        (
            Self {
                incoming_tx,
                incoming_rx,
                written_reports,
                responses_on_write,
                park_writes,
                drop_flag,
            },
            handle,
        )
    }
}

impl Drop for MockRawHidChannel {
    fn drop(&mut self) {
        if let Some(drop_flag) = self.drop_flag {
            drop_flag.store(true, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl RawHidChannel for MockRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xc539
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        self.written_reports.lock().unwrap().push(src.to_vec());
        if self.park_writes.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        let response = self.responses_on_write.lock().unwrap().pop_front();
        if let Some(response) = response {
            self.incoming_tx.send(response).await.unwrap();
        }

        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Sync + Send>> {
        let report = self.incoming_rx.recv().await.map_err(|_| mock_error())?;
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>> {
        unreachable!("mock declares HID++ support")
    }
}

fn short_msg(marker: u8) -> HidppMessage {
    HidppMessage::Short([0xff, marker, 0x10, marker, marker, marker])
}

fn leased_policy(id: u8, free: fn(u8)) -> SwIdPolicy {
    SwIdPolicy::Leased {
        id: RequestSwId::new(U4::from_lo(id)).unwrap(),
        free,
    }
}

fn record_sw_id_release(id: u8) {
    RELEASED_SW_IDS.lock().unwrap().push(id);
}

fn record_ordered_sw_id_release(_id: u8) {
    ORDERING_RELEASE_AFTER_RAW_DROP.store(
        ORDERING_RAW_CHANNEL_DROPPED.load(Ordering::SeqCst),
        Ordering::SeqCst,
    );
    ORDERING_RELEASE_COUNT.fetch_add(1, Ordering::SeqCst);
}

fn raw_report(msg: HidppMessage) -> Vec<u8> {
    let mut buf = [0u8; LONG_REPORT_LENGTH];
    let len = msg.write_raw(&mut buf);
    buf[..len].to_vec()
}

fn assert_pending_empty(channel: &HidppChannel) {
    assert!(channel.pending_messages.lock().unwrap().is_empty());
}

async fn wait_for_event_count(events: &Arc<Mutex<Vec<(HidppMessage, bool)>>>, count: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if events.lock().unwrap().len() >= count {
            return;
        }
        futures_timer::Delay::new(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for {count} listener events");
}

async fn wait_for_atomic_count(count: &AtomicUsize, expected: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if count.load(Ordering::SeqCst) >= expected {
            return;
        }
        futures_timer::Delay::new(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for atomic count {expected}");
}

fn mock_error() -> Box<dyn Error + Sync + Send> {
    Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "mock channel closed",
    ))
}
