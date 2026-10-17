//! The mock HID transport the crate's tests run a live channel on.
//!
//! [`MockRawHidChannel`] stands in for the HID node and [`MockRawHidHandle`]
//! is the test's side of it: it queues replies, injects incoming reports, and
//! records what was written. Tests across the crate use it, not only the
//! channel's own, so the module is `pub(crate)`.

use std::{
    collections::VecDeque,
    error::Error,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;

use super::{HidppChannel, HidppMessage, LONG_REPORT_LENGTH, RawHidChannel};

/// A live channel over the mock transport.
pub(crate) async fn channel_with_reader(raw: MockRawHidChannel) -> HidppChannel {
    HidppChannel::from_raw_channel(raw)
        .await
        .expect("the mock transport speaks HID++")
}

#[derive(Clone)]
pub(crate) struct MockRawHidHandle {
    incoming_tx: async_channel::Sender<Vec<u8>>,
    written_reports: Arc<Mutex<Vec<Vec<u8>>>>,
    responses_on_write: Arc<Mutex<VecDeque<Vec<u8>>>>,
    park_writes: Arc<AtomicBool>,
    fail_writes: Arc<AtomicBool>,
}

impl MockRawHidHandle {
    pub(crate) fn queue_response(&self, msg: HidppMessage) {
        self.responses_on_write
            .lock()
            .unwrap()
            .push_back(raw_report(msg));
    }

    pub(crate) async fn send_incoming(&self, msg: HidppMessage) {
        self.incoming_tx.send(raw_report(msg)).await.unwrap();
    }

    pub(crate) async fn send_incoming_raw(&self, report: Vec<u8>) {
        self.incoming_tx.send(report).await.unwrap();
    }

    pub(crate) fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written_reports.lock().unwrap().clone()
    }

    pub(crate) fn park_writes(&self) {
        self.park_writes.store(true, Ordering::SeqCst);
    }

    pub(crate) fn release_writes(&self) {
        self.park_writes.store(false, Ordering::SeqCst);
    }

    pub(crate) fn fail_writes(&self) {
        self.fail_writes.store(true, Ordering::SeqCst);
    }
}

pub(crate) struct MockRawHidChannel {
    incoming_tx: async_channel::Sender<Vec<u8>>,
    incoming_rx: async_channel::Receiver<Vec<u8>>,
    written_reports: Arc<Mutex<Vec<Vec<u8>>>>,
    responses_on_write: Arc<Mutex<VecDeque<Vec<u8>>>>,
    park_writes: Arc<AtomicBool>,
    fail_writes: Arc<AtomicBool>,
    report_support: (bool, bool),
    drop_flag: Option<&'static AtomicBool>,
}

impl MockRawHidChannel {
    pub(crate) fn new() -> (Self, MockRawHidHandle) {
        Self::with_drop_flag(None)
    }

    pub(crate) fn long_only() -> (Self, MockRawHidHandle) {
        Self::with_configuration(None, (false, true))
    }

    pub(super) fn with_drop_flag(
        drop_flag: Option<&'static AtomicBool>,
    ) -> (Self, MockRawHidHandle) {
        Self::with_configuration(drop_flag, (true, true))
    }

    fn with_configuration(
        drop_flag: Option<&'static AtomicBool>,
        report_support: (bool, bool),
    ) -> (Self, MockRawHidHandle) {
        let (incoming_tx, incoming_rx) = async_channel::unbounded();
        let written_reports = Arc::new(Mutex::new(Vec::new()));
        let responses_on_write = Arc::new(Mutex::new(VecDeque::new()));
        let park_writes = Arc::new(AtomicBool::new(false));
        let fail_writes = Arc::new(AtomicBool::new(false));

        let handle = MockRawHidHandle {
            incoming_tx: incoming_tx.clone(),
            written_reports: Arc::clone(&written_reports),
            responses_on_write: Arc::clone(&responses_on_write),
            park_writes: Arc::clone(&park_writes),
            fail_writes: Arc::clone(&fail_writes),
        };

        (
            Self {
                incoming_tx,
                incoming_rx,
                written_reports,
                responses_on_write,
                park_writes,
                fail_writes,
                report_support,
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
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(mock_error());
        }
        while self.park_writes.load(Ordering::SeqCst) {
            futures_timer::Delay::new(Duration::from_millis(1)).await;
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
        Some(self.report_support)
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Sync + Send>> {
        unreachable!("mock declares HID++ support")
    }
}

fn raw_report(msg: HidppMessage) -> Vec<u8> {
    let mut buf = [0u8; LONG_REPORT_LENGTH];
    let len = msg.write_raw(&mut buf);
    buf[..len].to_vec()
}

fn mock_error() -> Box<dyn Error + Sync + Send> {
    Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "mock channel closed",
    ))
}
