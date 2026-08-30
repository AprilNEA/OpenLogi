use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hidpp::channel::RawHidChannel;
use tokio::sync::mpsc;

use crate::backend::{BackendError, RawWriter};

use super::barrier::{RequestKey, ResponseGates};
use super::schema::{
    FixtureError, HidCassette, ReportSupport, RequestMatch, format_hex, normalize_hidpp20,
};

type Responder = Arc<dyn Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync>;

impl RequestKey {
    fn description(&self) -> String {
        match self {
            Self::Exact(bytes) => format!("exact:{}", format_hex(bytes)),
            Self::Hidpp20(bytes) => format!("hidpp20:{}", format_hex(bytes)),
        }
    }
}

struct PendingExchange {
    key: RequestKey,
    response: Option<Vec<u8>>,
    required: bool,
    consumed: bool,
}

struct CassetteRuntime {
    exchanges: Vec<PendingExchange>,
    queues: HashMap<RequestKey, VecDeque<usize>>,
    unmatched: Vec<ReplayMismatch>,
}

pub(super) struct CassetteState {
    runtime: Mutex<CassetteRuntime>,
}

impl CassetteState {
    pub(super) fn new(cassette: HidCassette) -> Result<Arc<Self>, FixtureError> {
        cassette.validate()?;
        let mut queues: HashMap<_, VecDeque<_>> = HashMap::new();
        let exchanges = cassette
            .exchanges
            .into_iter()
            .enumerate()
            .map(|(index, exchange)| {
                let key = RequestKey::from_exchange(exchange.request_match, &exchange.request);
                queues.entry(key.clone()).or_default().push_back(index);
                PendingExchange {
                    key,
                    response: exchange.response,
                    required: exchange.required,
                    consumed: false,
                }
            })
            .collect();
        Ok(Arc::new(Self {
            runtime: Mutex::new(CassetteRuntime {
                exchanges,
                queues,
                unmatched: Vec::new(),
            }),
        }))
    }

    fn respond(&self, actual: &[u8]) -> Result<Option<Vec<u8>>, FixtureError> {
        let normalized = normalize_hidpp20(actual);
        let exact = RequestKey::Exact(actual.to_vec());
        let hidpp20 = RequestKey::Hidpp20(normalized.clone());
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        let matched = runtime
            .queues
            .get_mut(&exact)
            .and_then(VecDeque::pop_front)
            .map(|index| (index, RequestMatch::Exact))
            .or_else(|| {
                runtime
                    .queues
                    .get_mut(&hidpp20)
                    .and_then(VecDeque::pop_front)
                    .map(|index| (index, RequestMatch::Hidpp20))
            });
        let Some((index, request_match)) = matched else {
            let mismatch = ReplayMismatch {
                actual: format_hex(actual),
                normalized: format_hex(&normalized),
            };
            runtime.unmatched.push(mismatch.clone());
            return Err(FixtureError::UnmatchedRequest {
                actual: mismatch.actual,
                normalized: mismatch.normalized,
            });
        };
        let exchange = &mut runtime.exchanges[index];
        exchange.consumed = true;
        let mut response = exchange.response.clone();
        if request_match == RequestMatch::Hidpp20
            && let Some(response) = response.as_mut()
        {
            rebind_software_id(response, actual[3] & 0x0f);
        }
        Ok(response)
    }

    pub(super) fn completion(&self, written_reports: Vec<Vec<u8>>) -> ReplayCompletion {
        let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        let unconsumed_required = runtime
            .exchanges
            .iter()
            .filter(|exchange| exchange.required && !exchange.consumed)
            .map(|exchange| exchange.key.description())
            .collect();
        let consumed_optional = runtime
            .exchanges
            .iter()
            .filter(|exchange| !exchange.required && exchange.consumed)
            .count();
        let unused_optional = runtime
            .exchanges
            .iter()
            .filter(|exchange| !exchange.required && !exchange.consumed)
            .count();
        ReplayCompletion {
            written_reports,
            unmatched_requests: runtime.unmatched.clone(),
            unconsumed_required,
            consumed_optional,
            unused_optional,
            channel_open_count: 0,
        }
    }
}

fn rebind_software_id(response: &mut [u8], software_id: u8) {
    if response[2] == 0xff {
        response[4] = response[4] & 0xf0 | software_id;
    } else {
        response[3] = response[3] & 0xf0 | software_id;
    }
}

/// One outgoing report that did not match a pending cassette exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayMismatch {
    /// Exact outgoing bytes as lowercase hex.
    pub actual: String,
    /// HID++ 2.0 candidate key with only the software-ID nibble cleared.
    pub normalized: String,
}

/// Current cassette consumption and write-capture state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayCompletion {
    /// Every report written through the channel, in observed order.
    pub written_reports: Vec<Vec<u8>>,
    /// Outgoing reports that matched no pending exchange.
    pub unmatched_requests: Vec<ReplayMismatch>,
    /// Required normalized request keys that remain unused.
    pub unconsumed_required: Vec<String>,
    /// Number of optional exchanges that were consumed.
    pub consumed_optional: usize,
    /// Number of optional exchanges that remain unused.
    pub unused_optional: usize,
    /// Number of times a replay backend opened this logical channel.
    ///
    /// Standalone [`ReplayRawHidChannel`] handles report zero because no
    /// backend open occurred.
    pub channel_open_count: usize,
}

impl ReplayCompletion {
    /// Whether no request mismatched and every required exchange was consumed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unmatched_requests.is_empty() && self.unconsumed_required.is_empty()
    }

    /// Fail with the remaining required request keys when replay is incomplete.
    pub fn require_complete(&self) -> Result<(), FixtureError> {
        if let Some(mismatch) = self.unmatched_requests.first() {
            return Err(FixtureError::UnmatchedRequest {
                actual: mismatch.actual.clone(),
                normalized: mismatch.normalized.clone(),
            });
        }
        if self.unconsumed_required.is_empty() {
            Ok(())
        } else {
            Err(FixtureError::UnconsumedExchanges {
                requests: self.unconsumed_required.clone(),
            })
        }
    }
}

/// Inspection and connection control for a [`ReplayRawHidChannel`].
#[derive(Clone)]
pub struct ReplayChannelHandle {
    cassette: Option<Arc<CassetteState>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    connected: Arc<AtomicBool>,
}

impl ReplayChannelHandle {
    pub(super) fn from_parts(
        cassette: Arc<CassetteState>,
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        connected: Arc<AtomicBool>,
    ) -> Self {
        Self {
            cassette: Some(cassette),
            written,
            connected,
        }
    }

    /// Every report written through this logical channel.
    #[must_use]
    pub fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Current strict cassette consumption and diagnostics.
    #[must_use]
    pub fn completion(&self) -> ReplayCompletion {
        self.cassette.as_ref().map_or_else(
            || ReplayCompletion {
                written_reports: self.written_reports(),
                unmatched_requests: Vec::new(),
                unconsumed_required: Vec::new(),
                consumed_optional: 0,
                unused_optional: 0,
                channel_open_count: 0,
            },
            |cassette| cassette.completion(self.written_reports()),
        )
    }

    /// Fail unless every required exchange was consumed without a mismatch.
    pub fn require_complete(&self) -> Result<(), FixtureError> {
        self.completion().require_complete()
    }

    /// Change whether this channel lifetime accepts further writes.
    pub fn set_connection(&self, connection: super::ChannelConnection) {
        self.connected.store(
            connection == super::ChannelConnection::Connected,
            Ordering::SeqCst,
        );
    }
}

/// A strict cassette-backed implementation of the production raw HID contract.
pub struct ReplayRawHidChannel {
    vendor_id: u16,
    product_id: u16,
    report_support: ReportSupport,
    incoming_tx: mpsc::UnboundedSender<Vec<u8>>,
    incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    cassette: Option<Arc<CassetteState>>,
    response_gates: Arc<ResponseGates>,
    responder: Option<Responder>,
    connected: Arc<AtomicBool>,
    fails: Option<fn(&[u8]) -> bool>,
}

impl ReplayRawHidChannel {
    /// Build one connected raw channel over `cassette`.
    pub fn new(
        cassette: HidCassette,
        vendor_id: u16,
        product_id: u16,
    ) -> Result<(Self, ReplayChannelHandle), FixtureError> {
        let report_support = cassette.report_support;
        let cassette = CassetteState::new(cassette)?;
        let written = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(AtomicBool::new(true));
        let response_gates = Arc::new(ResponseGates::default());
        let channel = Self::from_parts(
            vendor_id,
            product_id,
            report_support,
            Arc::clone(&cassette),
            Arc::clone(&written),
            Arc::clone(&connected),
            response_gates,
        );
        let handle = ReplayChannelHandle::from_parts(cassette, written, connected);
        Ok((channel, handle))
    }

    pub(super) fn from_parts(
        vendor_id: u16,
        product_id: u16,
        report_support: ReportSupport,
        cassette: Arc<CassetteState>,
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        connected: Arc<AtomicBool>,
        response_gates: Arc<ResponseGates>,
    ) -> Self {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        Self {
            vendor_id,
            product_id,
            report_support,
            incoming_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            written,
            cassette: Some(cassette),
            response_gates,
            responder: None,
            connected,
            fails: None,
        }
    }

    pub(super) fn incoming_sender(&self) -> mpsc::UnboundedSender<Vec<u8>> {
        self.incoming_tx.clone()
    }

    #[cfg(test)]
    pub(crate) fn with_responder(
        responder: fn(&[u8]) -> Option<Vec<u8>>,
    ) -> (Self, ReplayChannelHandle) {
        Self::build_scripted(responder, None)
    }

    #[cfg(test)]
    pub(crate) fn with_dynamic_responder(
        responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> (Self, ReplayChannelHandle) {
        Self::build_scripted(responder, None)
    }

    #[cfg(test)]
    pub(crate) fn with_failing_writes(
        responder: fn(&[u8]) -> Option<Vec<u8>>,
        fails: fn(&[u8]) -> bool,
    ) -> (Self, ReplayChannelHandle) {
        Self::build_scripted(responder, Some(fails))
    }

    #[cfg(test)]
    fn build_scripted(
        responder: impl Fn(&[u8]) -> Option<Vec<u8>> + Send + Sync + 'static,
        fails: Option<fn(&[u8]) -> bool>,
    ) -> (Self, ReplayChannelHandle) {
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(AtomicBool::new(true));
        (
            Self {
                vendor_id: 0x046d,
                product_id: 0xb35b,
                report_support: ReportSupport::ShortAndLong,
                incoming_tx,
                incoming_rx: tokio::sync::Mutex::new(incoming_rx),
                written: Arc::clone(&written),
                cassette: None,
                response_gates: Arc::new(ResponseGates::default()),
                responder: Some(Arc::new(responder)),
                connected: Arc::clone(&connected),
                fails,
            },
            ReplayChannelHandle {
                cassette: None,
                written,
                connected,
            },
        )
    }
}

#[hidpp::async_trait]
impl RawHidChannel for ReplayRawHidChannel {
    fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    fn product_id(&self) -> u16 {
        self.product_id
    }

    async fn write_report(&self, src: &[u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(src.to_vec());
        if !self.connected.load(Ordering::SeqCst) {
            return Err(disconnected_error());
        }
        if self.fails.is_some_and(|fails| fails(src)) {
            return Err(disconnected_error());
        }
        let response = if let Some(cassette) = &self.cassette {
            cassette.respond(src)?
        } else {
            self.responder.as_ref().and_then(|responder| responder(src))
        };
        if let Some(gate) = self.response_gates.take(src) {
            gate.request_written(self.incoming_tx.clone(), response);
            return Ok(src.len());
        }
        if let Some(response) = response {
            self.incoming_tx
                .send(response)
                .map_err(|_| disconnected_error())?;
        }
        Ok(src.len())
    }

    async fn read_report(&self, buf: &mut [u8]) -> Result<usize, Box<dyn Error + Send + Sync>> {
        let Some(report) = self.incoming_rx.lock().await.recv().await else {
            return Err(disconnected_error());
        };
        let len = report.len().min(buf.len());
        buf[..len].copy_from_slice(&report[..len]);
        Ok(len)
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn supports_short_long_hidpp(&self) -> Option<(bool, bool)> {
        Some(self.report_support.flags())
    }

    async fn get_report_descriptor(
        &self,
        _buf: &mut [u8],
    ) -> Result<usize, Box<dyn Error + Send + Sync>> {
        unreachable!("replay channel declares HID++ report support")
    }
}

fn disconnected_error() -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "replay HID channel is disconnected",
    ))
}

/// A raw output-report sink that records every successful write.
pub struct ReplayRawWriter {
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    connected: Arc<AtomicBool>,
}

impl ReplayRawWriter {
    /// Build one connected recording writer and its inspection handle.
    #[must_use]
    pub fn new() -> (Self, ReplayRawWriterHandle) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(AtomicBool::new(true));
        (
            Self {
                written: Arc::clone(&written),
                connected: Arc::clone(&connected),
            },
            ReplayRawWriterHandle { written, connected },
        )
    }

    pub(super) fn from_parts(
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        connected: Arc<AtomicBool>,
    ) -> Self {
        Self { written, connected }
    }
}

#[hidpp::async_trait]
impl RawWriter for ReplayRawWriter {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(BackendError::Disconnected);
        }
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(report.to_vec());
        Ok(())
    }
}

/// Inspection and connection control for a [`ReplayRawWriter`].
#[derive(Clone)]
pub struct ReplayRawWriterHandle {
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    connected: Arc<AtomicBool>,
}

impl ReplayRawWriterHandle {
    pub(super) fn from_parts(
        written: Arc<Mutex<Vec<Vec<u8>>>>,
        connected: Arc<AtomicBool>,
    ) -> Self {
        Self { written, connected }
    }

    /// Every output report accepted by this writer.
    #[must_use]
    pub fn written_reports(&self) -> Vec<Vec<u8>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Change whether the writer accepts further reports.
    pub fn set_connection(&self, connection: super::ChannelConnection) {
        self.connected.store(
            connection == super::ChannelConnection::Connected,
            Ordering::SeqCst,
        );
    }
}
