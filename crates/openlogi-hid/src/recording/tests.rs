use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hidpp::async_trait;
use hidpp::channel::{
    ChannelObservation, HidppChannel, HidppMessage, LONG_REPORT_LENGTH, RawHidChannel,
    RequestOutcome,
};
use openlogi_device::backend::{BackendError, NodeId, NodeInfo, RawWriter};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use super::*;

#[tokio::test]
async fn associates_concurrent_out_of_order_requests_and_retains_unmatched_evidence() {
    let recorder = NativeRecorder::new(64).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_channel(test_node()).unwrap();
    let (raw, handle) = FakeRawHidChannel::new();
    let channel = HidppChannel::from_raw_channel_with_observer(raw, capture.observer())
        .await
        .unwrap();
    capture.complete(RecordedChannelOpenOutcome::Opened {
        supports_short: channel.supports_short,
        supports_long: channel.supports_long,
    });
    drop(capture);

    let first_response = short_message(0x31);
    let second_response = short_message(0x41);
    let first = channel.send_with_timeout(
        short_message(0x11),
        move |candidate| *candidate == first_response,
        Duration::from_secs(1),
    );
    let second = channel.send_with_timeout(
        short_message(0x21),
        move |candidate| *candidate == second_response,
        Duration::from_secs(1),
    );
    let respond = async {
        handle.wait_for_writes(2).await;
        handle.send_message(second_response);
        handle.send_message(first_response);
    };

    let (first, second, ()) = tokio::join!(first, second, respond);
    assert_eq!(first.unwrap(), first_response);
    assert_eq!(second.unwrap(), second_response);

    let unmatched = short_message(0x51);
    handle.send_message(unmatched);
    handle.send_raw(vec![0x10, 0xff, 0x01]);
    wait_for_accepted(&recorder, 10).await;
    drop(channel);
    wait_for_accepted(&recorder, 11).await;

    let recording = recorder.finish().unwrap();
    assert_eq!(recording.channels.len(), 1);
    let channel = &recording.channels[0];
    assert_eq!(channel.node.name, "Test Receiver");
    assert_eq!(
        channel.node.serial_number.as_deref(),
        Some("unsanitized-test-serial")
    );
    assert!(channel.closed_at.is_some());
    assert_eq!(channel.requests.len(), 2);
    assert_eq!(
        channel
            .requests
            .iter()
            .map(|request| request.request_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_request_complete(&channel.requests[0], first_response);
    assert_request_complete(&channel.requests[1], second_response);

    let unmatched_raw = raw_report(unmatched);
    assert!(channel.unassociated.iter().any(|evidence| {
        matches!(
            &evidence.observation,
            ChannelObservation::IncomingReport {
                request_id: None,
                report,
            } if report.as_bytes() == unmatched_raw
        )
    }));
    assert!(channel.unassociated.iter().any(|evidence| {
        matches!(
            &evidence.observation,
            ChannelObservation::MalformedIncomingReport { report }
                if report.as_bytes() == [0x10, 0xff, 0x01]
        )
    }));
}

#[tokio::test]
async fn records_standalone_raw_writer_success_and_failure_separately() {
    let recorder = NativeRecorder::new(16).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_raw_writer(test_node()).unwrap();
    capture.complete(RecordedRawWriterOpenOutcome::Opened);
    let written = Arc::new(Mutex::new(Vec::new()));
    let writer = FakeRawWriter {
        written: Arc::clone(&written),
    };
    let mut writer = RecordingRawWriter::new(Box::new(writer), capture);

    writer.write_output_report(&[0x11, 0x22]).await.unwrap();
    let failure = writer.write_output_report(&[0xee, 0x33]).await.unwrap_err();
    assert_eq!(failure.to_string(), "fake raw write failed");
    drop(writer);

    let recording = recorder.finish().unwrap();
    assert!(recording.channels.is_empty());
    assert_eq!(recording.raw_writers.len(), 1);
    let raw_writer = &recording.raw_writers[0];
    assert!(raw_writer.closed_at.is_some());
    assert_eq!(
        raw_writer.open_outcome,
        RecordedRawWriterOpenOutcome::Opened
    );
    assert_eq!(raw_writer.writes.len(), 2);
    assert_eq!(raw_writer.writes[0].report.as_ref(), [0x11, 0x22]);
    assert_eq!(
        raw_writer.writes[0].outcome,
        RecordedRawWriteOutcome::Succeeded
    );
    assert_eq!(raw_writer.writes[1].report.as_ref(), [0xee, 0x33]);
    assert_eq!(
        raw_writer.writes[1].outcome,
        RecordedRawWriteOutcome::Failed("fake raw write failed".into())
    );
    assert_eq!(
        written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        [vec![0x11, 0x22], vec![0xee, 0x33]]
    );
}

#[tokio::test]
async fn channel_raw_report_is_observed_once_without_a_raw_writer_event() {
    let recorder = NativeRecorder::new(8).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_channel(test_node()).unwrap();
    let (raw, _handle) = FakeRawHidChannel::new();
    let channel = HidppChannel::from_raw_channel_with_observer(raw, capture.observer())
        .await
        .unwrap();
    capture.complete(RecordedChannelOpenOutcome::Opened {
        supports_short: channel.supports_short,
        supports_long: channel.supports_long,
    });
    drop(capture);

    let report = [0x12, 0x01, 0x02, 0x03];
    assert_eq!(
        channel.write_raw_report(&report).await.unwrap(),
        report.len()
    );
    wait_for_accepted(&recorder, 3).await;
    drop(channel);

    let recording = recorder.finish().unwrap();
    assert!(recording.raw_writers.is_empty());
    let outgoing = recording.channels[0]
        .unassociated
        .iter()
        .filter(|evidence| {
            matches!(
                &evidence.observation,
                ChannelObservation::OutgoingReport {
                    request_id: None,
                    report: observed,
                } if observed.as_bytes() == report
            )
        })
        .count();
    assert_eq!(outgoing, 1);
}

#[test]
fn overflow_fails_finalization_instead_of_returning_partial_evidence() {
    let recorder = NativeRecorder::new(1).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_channel(test_node()).unwrap();
    capture.complete(RecordedChannelOpenOutcome::NotHidpp);
    drop(capture);

    assert_eq!(
        recorder.finish().unwrap_err(),
        NativeRecordingError::Overflow { capacity: 1 }
    );
}

#[tokio::test]
async fn oversized_raw_report_reaches_transport_but_fails_capture() {
    let recorder = NativeRecorder::new(8).unwrap();
    let sink = recorder.sink();
    let mut capture = sink.begin_raw_writer(test_node()).unwrap();
    capture.complete(RecordedRawWriterOpenOutcome::Opened);
    let written = Arc::new(Mutex::new(Vec::new()));
    let mut writer = RecordingRawWriter::new(
        Box::new(FakeRawWriter {
            written: Arc::clone(&written),
        }),
        capture,
    );
    let report = [0x12; MAX_RECORDED_RAW_REPORT_LENGTH + 1];

    writer.write_output_report(&report).await.unwrap();
    drop(writer);

    assert_eq!(
        recorder.finish().unwrap_err(),
        NativeRecordingError::RawReportTooLong {
            length: MAX_RECORDED_RAW_REPORT_LENGTH + 1,
            max: MAX_RECORDED_RAW_REPORT_LENGTH,
        }
    );
    assert_eq!(
        written.lock().unwrap_or_else(PoisonError::into_inner)[0],
        report
    );
}

#[test]
fn premature_finalization_and_use_after_close_are_explicit_failures() {
    let recorder = NativeRecorder::new(8).unwrap();
    let sink = recorder.sink();
    let capture = sink.begin_channel(test_node()).unwrap();
    assert_eq!(
        recorder.finish().unwrap_err(),
        NativeRecordingError::ActiveProducers { count: 1 }
    );
    assert_eq!(
        sink.begin_channel(test_node()).err().unwrap(),
        NativeRecordingError::Closed
    );
    drop(capture);
}

#[tokio::test]
async fn default_channel_construction_remains_unobserved_and_writes_normally() {
    let (raw, handle) = FakeRawHidChannel::new();
    let channel = crate::transport::hidpp_channel_from_raw(raw, None)
        .await
        .unwrap();

    channel.send_and_forget(short_message(0x61)).await.unwrap();
    handle.wait_for_writes(1).await;
    assert_eq!(handle.written_reports().len(), 1);
}

fn assert_request_complete(request: &RecordedRequest, response: HidppMessage) {
    assert!(
        request
            .facts
            .iter()
            .any(|fact| { matches!(fact, RecordedRequestFact::OutgoingReport { .. }) })
    );
    let response = raw_report(response);
    assert!(request.facts.iter().any(|fact| {
        matches!(
            fact,
            RecordedRequestFact::IncomingReport { report, .. }
                if report.as_bytes() == response
        )
    }));
    assert!(request.facts.iter().any(|fact| {
        matches!(
            fact,
            RecordedRequestFact::Outcome {
                outcome: RequestOutcome::Succeeded,
                ..
            }
        )
    }));
}

async fn wait_for_accepted(recorder: &NativeRecorder, expected: usize) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        if recorder
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .accepted
            >= expected
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {expected} recorder events");
}

fn test_node() -> NodeInfo {
    NodeInfo {
        id: NodeId::from("native-test-node".to_owned()),
        vendor_id: 0x046d,
        product_id: 0xc548,
        usage_page: 0xff00,
        usage_id: 0x0002,
        name: "Test Receiver".to_owned(),
        manufacturer: Some("Logitech".to_owned()),
        serial_number: Some("unsanitized-test-serial".to_owned()),
    }
}

fn short_message(marker: u8) -> HidppMessage {
    HidppMessage::Short([0xff, marker, 0x10, marker, marker, marker])
}

fn raw_report(message: HidppMessage) -> Vec<u8> {
    let mut report = [0; LONG_REPORT_LENGTH];
    let len = message.write_raw(&mut report);
    report[..len].to_vec()
}

struct FakeRawHidChannel {
    incoming: AsyncMutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

struct FakeRawHidHandle {
    incoming: mpsc::UnboundedSender<Vec<u8>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeRawHidChannel {
    fn new() -> (Self, FakeRawHidHandle) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                incoming: AsyncMutex::new(receiver),
                written: Arc::clone(&written),
            },
            FakeRawHidHandle {
                incoming: sender,
                written,
            },
        )
    }
}

impl FakeRawHidHandle {
    fn send_message(&self, message: HidppMessage) {
        self.send_raw(raw_report(message));
    }

    fn send_raw(&self, report: Vec<u8>) {
        self.incoming.send(report).unwrap();
    }

    fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn wait_for_writes(&self, expected: usize) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            if self
                .written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
                >= expected
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {expected} raw writes");
    }
}

#[async_trait]
impl RawHidChannel for FakeRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xc548
    }

    async fn write_report(&self, report: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report.to_vec());
        Ok(report.len())
    }

    async fn read_report(&self, buffer: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming.lock().await.recv().await else {
            return std::future::pending().await;
        };
        if report.len() > buffer.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "report too long").into());
        }
        buffer[..report.len()].copy_from_slice(&report);
        Ok(report.len())
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some((true, true))
    }

    async fn get_report_descriptor(
        &self,
        _buffer: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "descriptor not needed").into())
    }
}

struct FakeRawWriter {
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[async_trait]
impl RawWriter for FakeRawWriter {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report.to_vec());
        if report.first() == Some(&0xee) {
            Err(BackendError::Backend("fake raw write failed".into()))
        } else {
            Ok(())
        }
    }
}
