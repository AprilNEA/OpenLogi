use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;

use hidpp::channel::ChannelObservation;
use openlogi_device::backend::NodeInfo;

use super::{
    NativeRecording, RecordedChannel, RecordedChannelEvidence, RecordedChannelId,
    RecordedChannelOpenOutcome, RecordedRawWrite, RecordedRawWriteOutcome, RecordedRawWriter,
    RecordedRawWriterId, RecordedRawWriterOpenOutcome, RecordedRequest, RecordedRequestFact,
    RecordingSequence,
};

pub(super) enum RecorderCommand {
    ChannelStarted {
        sequence: RecordingSequence,
        id: RecordedChannelId,
        node: NodeInfo,
    },
    ChannelOutcome {
        sequence: RecordingSequence,
        id: RecordedChannelId,
        outcome: RecordedChannelOpenOutcome,
    },
    ChannelObservation {
        sequence: RecordingSequence,
        id: RecordedChannelId,
        observation: ChannelObservation,
    },
    ChannelClosed {
        sequence: RecordingSequence,
        id: RecordedChannelId,
    },
    RawWriterStarted {
        sequence: RecordingSequence,
        id: RecordedRawWriterId,
        node: NodeInfo,
    },
    RawWriterOutcome {
        sequence: RecordingSequence,
        id: RecordedRawWriterId,
        outcome: RecordedRawWriterOpenOutcome,
    },
    RawWrite {
        sequence: RecordingSequence,
        id: RecordedRawWriterId,
        report: Box<[u8]>,
        outcome: RecordedRawWriteOutcome,
    },
    RawWriterClosed {
        sequence: RecordingSequence,
        id: RecordedRawWriterId,
    },
    Finish,
}

#[derive(Default)]
pub(super) struct Accumulator {
    channels: BTreeMap<RecordedChannelId, ChannelBuilder>,
    raw_writers: BTreeMap<RecordedRawWriterId, RawWriterBuilder>,
}

impl Accumulator {
    pub(super) fn run(mut self, receiver: &Receiver<RecorderCommand>) -> NativeRecording {
        while let Ok(command) = receiver.recv() {
            match command {
                RecorderCommand::ChannelStarted { sequence, id, node } => {
                    self.channels
                        .insert(id, ChannelBuilder::new(sequence, node));
                }
                RecorderCommand::ChannelOutcome {
                    sequence,
                    id,
                    outcome,
                } => {
                    if let Some(channel) = self.channels.get_mut(&id) {
                        channel.open_outcome = Some((sequence, outcome));
                    }
                }
                RecorderCommand::ChannelObservation {
                    sequence,
                    id,
                    observation,
                } => {
                    if let Some(channel) = self.channels.get_mut(&id) {
                        channel.observe(sequence, observation);
                    }
                }
                RecorderCommand::ChannelClosed { sequence, id } => {
                    if let Some(channel) = self.channels.get_mut(&id) {
                        channel.closed = Some(sequence);
                    }
                }
                RecorderCommand::RawWriterStarted { sequence, id, node } => {
                    self.raw_writers
                        .insert(id, RawWriterBuilder::new(sequence, node));
                }
                RecorderCommand::RawWriterOutcome {
                    sequence,
                    id,
                    outcome,
                } => {
                    if let Some(writer) = self.raw_writers.get_mut(&id) {
                        writer.open_outcome = Some((sequence, outcome));
                    }
                }
                RecorderCommand::RawWrite {
                    sequence,
                    id,
                    report,
                    outcome,
                } => {
                    if let Some(writer) = self.raw_writers.get_mut(&id) {
                        writer.writes.push(RecordedRawWrite {
                            sequence,
                            report,
                            outcome,
                        });
                    }
                }
                RecorderCommand::RawWriterClosed { sequence, id } => {
                    if let Some(writer) = self.raw_writers.get_mut(&id) {
                        writer.closed = Some(sequence);
                    }
                }
                RecorderCommand::Finish => break,
            }
        }
        NativeRecording {
            channels: self
                .channels
                .into_iter()
                .map(|(id, channel)| channel.finish(id))
                .collect(),
            raw_writers: self
                .raw_writers
                .into_iter()
                .map(|(id, writer)| writer.finish(id))
                .collect(),
        }
    }
}

struct ChannelBuilder {
    started: RecordingSequence,
    node: NodeInfo,
    open_outcome: Option<(RecordingSequence, RecordedChannelOpenOutcome)>,
    requests: BTreeMap<u64, Vec<RecordedRequestFact>>,
    unassociated: Vec<RecordedChannelEvidence>,
    closed: Option<RecordingSequence>,
}

impl ChannelBuilder {
    fn new(started: RecordingSequence, node: NodeInfo) -> Self {
        Self {
            started,
            node,
            open_outcome: None,
            requests: BTreeMap::new(),
            unassociated: Vec::new(),
            closed: None,
        }
    }

    fn observe(&mut self, sequence: RecordingSequence, observation: ChannelObservation) {
        let associated = match observation {
            ChannelObservation::OutgoingReport {
                request_id: Some(request_id),
                report,
            } => Some((
                request_id,
                RecordedRequestFact::OutgoingReport { sequence, report },
            )),
            ChannelObservation::IncomingReport {
                request_id: Some(request_id),
                report,
            } => Some((
                request_id,
                RecordedRequestFact::IncomingReport { sequence, report },
            )),
            ChannelObservation::RequestOutcome {
                request_id,
                outcome,
            } => Some((
                request_id,
                RecordedRequestFact::Outcome { sequence, outcome },
            )),
            unassociated => {
                self.unassociated.push(RecordedChannelEvidence {
                    sequence,
                    observation: unassociated,
                });
                None
            }
        };
        if let Some((request_id, fact)) = associated {
            self.requests.entry(request_id).or_default().push(fact);
        }
    }

    fn finish(self, id: RecordedChannelId) -> RecordedChannel {
        let (open_outcome_at, open_outcome) = self
            .open_outcome
            .unwrap_or((self.started, RecordedChannelOpenOutcome::Cancelled));
        RecordedChannel {
            id,
            node: self.node,
            started_at: self.started,
            open_outcome,
            open_outcome_at,
            requests: self
                .requests
                .into_iter()
                .map(|(request_id, facts)| RecordedRequest { request_id, facts })
                .collect(),
            unassociated: self.unassociated,
            closed_at: self.closed,
        }
    }
}

struct RawWriterBuilder {
    started: RecordingSequence,
    node: NodeInfo,
    open_outcome: Option<(RecordingSequence, RecordedRawWriterOpenOutcome)>,
    writes: Vec<RecordedRawWrite>,
    closed: Option<RecordingSequence>,
}

impl RawWriterBuilder {
    fn new(started: RecordingSequence, node: NodeInfo) -> Self {
        Self {
            started,
            node,
            open_outcome: None,
            writes: Vec::new(),
            closed: None,
        }
    }

    fn finish(self, id: RecordedRawWriterId) -> RecordedRawWriter {
        let (open_outcome_at, open_outcome) = self
            .open_outcome
            .unwrap_or((self.started, RecordedRawWriterOpenOutcome::Cancelled));
        RecordedRawWriter {
            id,
            node: self.node,
            started_at: self.started,
            open_outcome,
            open_outcome_at,
            writes: self.writes,
            closed_at: self.closed,
        }
    }
}
