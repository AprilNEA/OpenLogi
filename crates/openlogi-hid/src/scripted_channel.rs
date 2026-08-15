//! A scripted HID++ transport for tests: it answers requests from a per-device
//! responder instead of talking to hardware.
//!
//! Shared because more than one module needs a device with a feature table of
//! its choosing — `write` drives DPI and lighting against one, `host_switch`
//! needs a keyboard whose host slots it can dictate. Each module keeps its own
//! responder; only the plumbing lives here.

use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::channel::RawHidChannel;
use tokio::sync::mpsc;

#[derive(Clone)]
pub(crate) struct ScriptedRawHidHandle {
    written: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ScriptedRawHidHandle {
    pub(crate) fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Answers a HID++ request as a particular scripted device would.
pub(crate) type Responder = fn(&[u8]) -> Option<Vec<u8>>;

pub(crate) struct ScriptedRawHidChannel {
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    responder: Responder,
}

impl ScriptedRawHidChannel {
    /// A channel answering as `responder`'s device.
    pub(crate) fn with_responder(responder: Responder) -> (Self, ScriptedRawHidHandle) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written: Arc::clone(&written),
                responder,
            },
            ScriptedRawHidHandle { written },
        )
    }
}

#[hidpp::async_trait]
impl RawHidChannel for ScriptedRawHidChannel {
    fn vendor_id(&self) -> u16 {
        0x046d
    }

    fn product_id(&self) -> u16 {
        0xb35b
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(src.to_vec());
        if let Some(response) = (self.responder)(src) {
            self.incoming_tx.send(response).map_err(|_| mock_error())?;
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return Err(mock_error());
        };
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
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        unreachable!("scripted channel declares HID++ support")
    }
}

pub(crate) fn mock_error() -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "scripted HID channel closed",
    ))
}
