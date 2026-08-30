//! Bounded in-memory capture of native HID traffic.
//!
//! This module records transport evidence only. It deliberately performs no
//! sanitization, serialization, file I/O, or semantic expectation generation;
//! a later CLI layer owns those decisions.

use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use hidpp::async_trait;
use hidpp::channel::{ChannelObservation, ChannelObserver};
use openlogi_device::backend::{BackendError, HidBackend, NodeInfo, RawWriter};

mod accumulator;
mod model;

use accumulator::{Accumulator, RecorderCommand};
pub use model::*;

#[cfg(test)]
mod tests;

/// Bounded, concurrency-safe native HID evidence recorder.
///
/// Observer callbacks only take a short in-memory lock and perform a
/// non-blocking enqueue. Total accepted evidence is capped by `capacity`, even
/// though a worker drains the queue concurrently. Overflow or premature
/// closure makes [`Self::finish`] fail rather than returning partial evidence.
pub struct NativeRecorder {
    shared: Arc<RecorderShared>,
    worker: Mutex<Option<JoinHandle<NativeRecording>>>,
}

impl NativeRecorder {
    /// Create a recorder retaining at most `capacity` events in total.
    pub fn new(capacity: usize) -> Result<Self, NativeRecordingError> {
        if capacity == 0 {
            return Err(NativeRecordingError::InvalidCapacity);
        }
        let (sender, receiver) = sync_channel(capacity);
        let worker = thread::Builder::new()
            .name("openlogi-native-recorder".into())
            .spawn(move || Accumulator::default().run(&receiver))
            .map_err(|error| NativeRecordingError::WorkerStart(error.to_string()))?;
        Ok(Self {
            shared: Arc::new(RecorderShared {
                sender,
                state: Mutex::new(RecorderState::new(capacity)),
            }),
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Construct an explicit recording facade over the process-wide native HID
    /// manager and device-I/O gate.
    ///
    /// The facade owns a separate enumeration handle cache, but it does not
    /// construct another native HID manager. Normal [`crate::host::backend`]
    /// callers remain unobserved.
    #[must_use]
    pub fn backend(&self) -> Arc<dyn HidBackend> {
        crate::transport::recording_backend(RecordingSink {
            shared: Arc::clone(&self.shared),
        })
    }

    /// Close the recorder, join its accumulator, and return grouped evidence.
    ///
    /// All channels, raw writers, and recording backends should be dropped
    /// first. Finalizing with a live channel observer or raw writer fails so a
    /// caller cannot mistake an incomplete channel lifetime for a full capture.
    pub fn finish(&self) -> Result<NativeRecording, NativeRecordingError> {
        let failure = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.finalized {
                return Err(NativeRecordingError::AlreadyFinalized);
            }
            state.finalized = true;
            state.closed = true;
            state.failure.clone().or_else(|| {
                (state.active_producers != 0).then_some(NativeRecordingError::ActiveProducers {
                    count: state.active_producers,
                })
            })
        };

        let recording = self.stop_worker()?;
        match failure {
            Some(error) => Err(error),
            None => Ok(recording),
        }
    }

    fn stop_worker(&self) -> Result<NativeRecording, NativeRecordingError> {
        let sent = self.shared.sender.send(RecorderCommand::Finish).is_ok();
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or(NativeRecordingError::AlreadyFinalized)?;
        let recording = worker
            .join()
            .map_err(|_| NativeRecordingError::WorkerPanicked)?;
        if sent {
            Ok(recording)
        } else {
            Err(NativeRecordingError::WorkerUnavailable)
        }
    }

    #[cfg(test)]
    fn sink(&self) -> RecordingSink {
        RecordingSink {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for NativeRecorder {
    fn drop(&mut self) {
        let has_worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        if !has_worker {
            return;
        }
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            state.finalized = true;
        }
        let _ = self.stop_worker();
    }
}

#[derive(Clone)]
pub(crate) struct RecordingSink {
    shared: Arc<RecorderShared>,
}

impl RecordingSink {
    pub(crate) fn begin_channel(
        &self,
        node: NodeInfo,
    ) -> Result<ChannelCapture, NativeRecordingError> {
        let id =
            {
                let mut state = self.lock_state();
                state.ensure_open()?;
                let id = RecordedChannelId(state.next_channel_id);
                state.next_channel_id = state.next_channel_id.saturating_add(1);
                self.shared.enqueue_locked(&mut state, |sequence| {
                    RecorderCommand::ChannelStarted { sequence, id, node }
                })?;
                state.active_producers = state.active_producers.saturating_add(1);
                id
            };
        Ok(ChannelCapture {
            observer: Arc::new(RecordingChannelObserver {
                id,
                shared: Arc::clone(&self.shared),
            }),
            completed: false,
        })
    }

    pub(crate) fn begin_raw_writer(
        &self,
        node: NodeInfo,
    ) -> Result<RawWriterCapture, NativeRecordingError> {
        let id = {
            let mut state = self.lock_state();
            state.ensure_open()?;
            let id = RecordedRawWriterId(state.next_raw_writer_id);
            state.next_raw_writer_id = state.next_raw_writer_id.saturating_add(1);
            self.shared.enqueue_locked(&mut state, |sequence| {
                RecorderCommand::RawWriterStarted { sequence, id, node }
            })?;
            state.active_producers = state.active_producers.saturating_add(1);
            id
        };
        Ok(RawWriterCapture {
            id,
            shared: Arc::clone(&self.shared),
            completed: false,
        })
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RecorderState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct ChannelCapture {
    observer: Arc<RecordingChannelObserver>,
    completed: bool,
}

impl ChannelCapture {
    pub(crate) fn observer(&self) -> Arc<dyn ChannelObserver> {
        Arc::clone(&self.observer) as Arc<dyn ChannelObserver>
    }

    pub(crate) fn complete(&mut self, outcome: RecordedChannelOpenOutcome) {
        self.completed = true;
        self.observer
            .shared
            .channel_outcome(self.observer.id, outcome);
    }
}

impl Drop for ChannelCapture {
    fn drop(&mut self) {
        if !self.completed {
            self.observer
                .shared
                .channel_outcome(self.observer.id, RecordedChannelOpenOutcome::Cancelled);
        }
    }
}

struct RecordingChannelObserver {
    id: RecordedChannelId,
    shared: Arc<RecorderShared>,
}

impl ChannelObserver for RecordingChannelObserver {
    fn observe(&self, observation: ChannelObservation) {
        self.shared.channel_observation(self.id, observation);
    }
}

impl Drop for RecordingChannelObserver {
    fn drop(&mut self) {
        self.shared.finish_channel(self.id);
    }
}

pub(crate) struct RawWriterCapture {
    id: RecordedRawWriterId,
    shared: Arc<RecorderShared>,
    completed: bool,
}

impl RawWriterCapture {
    pub(crate) fn complete(&mut self, outcome: RecordedRawWriterOpenOutcome) {
        self.completed = true;
        self.shared.raw_writer_outcome(self.id, outcome);
    }

    fn begin_write(&self, report: &[u8]) -> Option<PendingRawWrite> {
        if report.len() > MAX_RECORDED_RAW_REPORT_LENGTH {
            self.shared.fail(NativeRecordingError::RawReportTooLong {
                length: report.len(),
                max: MAX_RECORDED_RAW_REPORT_LENGTH,
            });
            return None;
        }
        Some(PendingRawWrite {
            id: self.id,
            shared: Arc::clone(&self.shared),
            report: Some(report.into()),
        })
    }
}

impl Drop for RawWriterCapture {
    fn drop(&mut self) {
        if !self.completed {
            self.shared
                .raw_writer_outcome(self.id, RecordedRawWriterOpenOutcome::Cancelled);
        }
        self.shared.finish_raw_writer(self.id);
    }
}

pub(crate) struct RecordingRawWriter {
    inner: Box<dyn RawWriter>,
    capture: RawWriterCapture,
}

impl RecordingRawWriter {
    pub(crate) fn new(inner: Box<dyn RawWriter>, capture: RawWriterCapture) -> Self {
        Self { inner, capture }
    }
}

#[async_trait]
impl RawWriter for RecordingRawWriter {
    async fn write_output_report(&mut self, report: &[u8]) -> Result<(), BackendError> {
        let pending = self.capture.begin_write(report);
        let result = self.inner.write_output_report(report).await;
        if let Some(pending) = pending {
            let outcome = match &result {
                Ok(()) => RecordedRawWriteOutcome::Succeeded,
                Err(error) => RecordedRawWriteOutcome::Failed(error.to_string()),
            };
            pending.complete(outcome);
        }
        result
    }
}

struct PendingRawWrite {
    id: RecordedRawWriterId,
    shared: Arc<RecorderShared>,
    report: Option<Box<[u8]>>,
}

impl PendingRawWrite {
    fn complete(mut self, outcome: RecordedRawWriteOutcome) {
        if let Some(report) = self.report.take() {
            self.shared.raw_write(self.id, report, outcome);
        }
    }
}

impl Drop for PendingRawWrite {
    fn drop(&mut self) {
        if let Some(report) = self.report.take() {
            self.shared
                .raw_write(self.id, report, RecordedRawWriteOutcome::Cancelled);
        }
    }
}

struct RecorderShared {
    sender: SyncSender<RecorderCommand>,
    state: Mutex<RecorderState>,
}

impl RecorderShared {
    fn enqueue_locked(
        &self,
        state: &mut RecorderState,
        command: impl FnOnce(RecordingSequence) -> RecorderCommand,
    ) -> Result<(), NativeRecordingError> {
        state.ensure_open()?;
        if state.accepted == state.capacity {
            let error = NativeRecordingError::Overflow {
                capacity: state.capacity,
            };
            state.failure = Some(error.clone());
            return Err(error);
        }
        let sequence = RecordingSequence(state.accepted.saturating_add(1) as u64);
        match self.sender.try_send(command(sequence)) {
            Ok(()) => {
                state.accepted = state.accepted.saturating_add(1);
                Ok(())
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                let error = NativeRecordingError::WorkerUnavailable;
                state.failure = Some(error.clone());
                Err(error)
            }
        }
    }

    fn channel_outcome(&self, id: RecordedChannelId, outcome: RecordedChannelOpenOutcome) {
        let mut state = self.lock_state();
        let _ = self.enqueue_locked(&mut state, |sequence| RecorderCommand::ChannelOutcome {
            sequence,
            id,
            outcome,
        });
    }

    fn channel_observation(&self, id: RecordedChannelId, observation: ChannelObservation) {
        let mut state = self.lock_state();
        let _ = self.enqueue_locked(&mut state, |sequence| RecorderCommand::ChannelObservation {
            sequence,
            id,
            observation,
        });
    }

    fn finish_channel(&self, id: RecordedChannelId) {
        let mut state = self.lock_state();
        let _ = self.enqueue_locked(&mut state, |sequence| RecorderCommand::ChannelClosed {
            sequence,
            id,
        });
        state.active_producers = state.active_producers.saturating_sub(1);
    }

    fn raw_writer_outcome(&self, id: RecordedRawWriterId, outcome: RecordedRawWriterOpenOutcome) {
        let mut state = self.lock_state();
        let _ = self.enqueue_locked(&mut state, |sequence| RecorderCommand::RawWriterOutcome {
            sequence,
            id,
            outcome,
        });
    }

    fn raw_write(
        &self,
        id: RecordedRawWriterId,
        report: Box<[u8]>,
        outcome: RecordedRawWriteOutcome,
    ) {
        let mut state = self.lock_state();
        let _ = self.enqueue_locked(&mut state, |sequence| RecorderCommand::RawWrite {
            sequence,
            id,
            report,
            outcome,
        });
    }

    fn finish_raw_writer(&self, id: RecordedRawWriterId) {
        let mut state = self.lock_state();
        let _ = self.enqueue_locked(&mut state, |sequence| RecorderCommand::RawWriterClosed {
            sequence,
            id,
        });
        state.active_producers = state.active_producers.saturating_sub(1);
    }

    fn fail(&self, error: NativeRecordingError) {
        let mut state = self.lock_state();
        if !state.closed && state.failure.is_none() {
            state.failure = Some(error);
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RecorderState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct RecorderState {
    capacity: usize,
    accepted: usize,
    next_channel_id: u64,
    next_raw_writer_id: u64,
    active_producers: usize,
    failure: Option<NativeRecordingError>,
    closed: bool,
    finalized: bool,
}

impl RecorderState {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            accepted: 0,
            next_channel_id: 1,
            next_raw_writer_id: 1,
            active_producers: 0,
            failure: None,
            closed: false,
            finalized: false,
        }
    }

    fn ensure_open(&self) -> Result<(), NativeRecordingError> {
        if self.closed {
            return Err(NativeRecordingError::Closed);
        }
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}
