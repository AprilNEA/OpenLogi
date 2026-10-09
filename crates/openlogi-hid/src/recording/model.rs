use hidpp::channel::{ChannelObservation, ObservedReport, RequestOutcome};
use openlogi_device::backend::NodeInfo;
use thiserror::Error;

/// Largest standalone raw output report retained by the native recorder.
///
/// OpenLogi's standalone Litra reports are 20 bytes and its very-long HID++
/// lighting reports are 64 bytes. Reports larger than this still reach the
/// underlying writer, but make capture finalization fail rather than allowing
/// unbounded recorder memory.
pub const MAX_RECORDED_RAW_REPORT_LENGTH: usize = 64;

/// Sequence assigned when the recorder accepts one piece of evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordingSequence(pub(super) u64);

impl RecordingSequence {
    /// Return the one-based sequence number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one native HID++ channel lifetime within a recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordedChannelId(pub(super) u64);

impl RecordedChannelId {
    /// Return the recorder-local channel identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one standalone native raw-writer lifetime within a recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordedRawWriterId(pub(super) u64);

impl RecordedRawWriterId {
    /// Return the recorder-local raw-writer identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Result of trying to construct one native HID++ channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordedChannelOpenOutcome {
    /// The channel opened and reported these HID++ widths.
    Opened {
        /// Whether short (`0x10`) HID++ reports are supported.
        supports_short: bool,
        /// Whether long (`0x11`) HID++ reports are supported.
        supports_long: bool,
    },
    /// The raw collection did not support HID++.
    NotHidpp,
    /// Native construction failed with this host-facing error.
    Failed(String),
    /// The construction future was cancelled before producing an outcome.
    Cancelled,
}

/// One request-associated fact, retained without depending on callback order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordedRequestFact {
    /// The post-normalization report submitted to the raw transport.
    OutgoingReport {
        /// When the recorder accepted this fact.
        sequence: RecordingSequence,
        /// Exact report bytes, including report ID.
        report: ObservedReport,
    },
    /// The incoming report matched to this request by `HidppChannel`.
    IncomingReport {
        /// When the recorder accepted this fact.
        sequence: RecordingSequence,
        /// Exact report bytes, including report ID.
        report: ObservedReport,
    },
    /// The request's terminal outcome.
    Outcome {
        /// When the recorder accepted this fact.
        sequence: RecordingSequence,
        /// How the request ended.
        outcome: RequestOutcome,
    },
}

/// All observed facts associated with one channel request ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Recorder-local request ID assigned by `HidppChannel`.
    pub request_id: u64,
    /// Facts ordered by their recorder sequence.
    pub facts: Vec<RecordedRequestFact>,
}

/// A channel observation with no request ID to associate it with.
///
/// This retains fire-and-forget/raw channel writes, unmatched incoming reports,
/// malformed incoming reports, and any future observation variant verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedChannelEvidence {
    /// When the recorder accepted this evidence.
    pub sequence: RecordingSequence,
    /// The original channel observation.
    pub observation: ChannelObservation,
}

/// One complete native HID++ channel lifetime.
#[derive(Debug)]
pub struct RecordedChannel {
    /// Recorder-local channel identity.
    pub id: RecordedChannelId,
    /// Unsanitized host node metadata captured for later grouping.
    pub node: NodeInfo,
    /// When construction of this channel lifetime began.
    pub started_at: RecordingSequence,
    /// Result of constructing the channel.
    pub open_outcome: RecordedChannelOpenOutcome,
    /// When the construction outcome was accepted.
    pub open_outcome_at: RecordingSequence,
    /// Request facts sorted by request ID, independent of callback order.
    pub requests: Vec<RecordedRequest>,
    /// Evidence that had no request ID.
    pub unassociated: Vec<RecordedChannelEvidence>,
    /// When every observer for this channel lifetime was dropped.
    pub closed_at: Option<RecordingSequence>,
}

/// Outcome of one standalone raw output write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordedRawWriteOutcome {
    /// The underlying native writer accepted the report.
    Succeeded,
    /// The underlying native writer rejected the report with this error.
    Failed(String),
    /// The write future was cancelled before the native writer completed.
    Cancelled,
}

/// One standalone raw output write that bypassed `HidppChannel`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedRawWrite {
    /// When the recorder accepted this write outcome.
    pub sequence: RecordingSequence,
    /// Exact output report bytes, including report ID.
    pub report: Box<[u8]>,
    /// Result of the native write.
    pub outcome: RecordedRawWriteOutcome,
}

/// Result of trying to construct one native standalone raw writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordedRawWriterOpenOutcome {
    /// The native writer opened successfully.
    Opened,
    /// Native construction failed with this host-facing error.
    Failed(String),
    /// The construction future was cancelled before producing an outcome.
    Cancelled,
}

/// One complete standalone native raw-writer lifetime.
#[derive(Debug)]
pub struct RecordedRawWriter {
    /// Recorder-local writer identity.
    pub id: RecordedRawWriterId,
    /// Unsanitized host node metadata captured for later grouping.
    pub node: NodeInfo,
    /// When construction of this writer lifetime began.
    pub started_at: RecordingSequence,
    /// Result of constructing the writer.
    pub open_outcome: RecordedRawWriterOpenOutcome,
    /// When the construction outcome was accepted.
    pub open_outcome_at: RecordingSequence,
    /// Raw writes in accepted sequence order.
    pub writes: Vec<RecordedRawWrite>,
    /// When the recording wrapper was dropped.
    pub closed_at: Option<RecordingSequence>,
}

/// Deterministic finalized view of native HID evidence.
///
/// Channels and raw writers are sorted by recorder-local lifetime ID. Requests
/// within a channel are sorted by request ID; request facts and unassociated
/// evidence retain their accepted sequence.
#[derive(Debug, Default)]
pub struct NativeRecording {
    /// Recorded HID++ channel lifetimes.
    pub channels: Vec<RecordedChannel>,
    /// Recorded standalone raw-writer lifetimes.
    pub raw_writers: Vec<RecordedRawWriter>,
}

/// Failure to create, retain, close, or finalize a native recording.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NativeRecordingError {
    /// A recorder must retain at least one event.
    #[error("native recorder capacity must be at least one event")]
    InvalidCapacity,
    /// The accumulator thread could not be started.
    #[error("could not start native recorder worker: {0}")]
    WorkerStart(String),
    /// The fixed event capacity was exhausted.
    #[error("native recorder overflowed its {capacity}-event capacity")]
    Overflow {
        /// Configured maximum retained event count.
        capacity: usize,
    },
    /// A standalone raw report exceeded the recorder's fixed report bound.
    #[error("raw report length {length} exceeds recording limit {max}")]
    RawReportTooLong {
        /// Attempted report length.
        length: usize,
        /// Maximum retained raw report length.
        max: usize,
    },
    /// Capture was finalized while producers were still alive.
    #[error("native recorder finalized with {count} active producer(s)")]
    ActiveProducers {
        /// Number of open channel observers and raw writers.
        count: usize,
    },
    /// An operation attempted to record after finalization began.
    #[error("native recorder is closed")]
    Closed,
    /// The accumulator thread stopped before finalization.
    #[error("native recorder worker stopped unexpectedly")]
    WorkerUnavailable,
    /// The accumulator thread panicked.
    #[error("native recorder worker panicked")]
    WorkerPanicked,
    /// The recorder had already been finalized.
    #[error("native recorder was already finalized")]
    AlreadyFinalized,
}
