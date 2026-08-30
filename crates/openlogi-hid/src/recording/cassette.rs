//! Strict cassette construction from one completed recorded channel.

use std::collections::BTreeSet;

use hidpp::channel::RequestOutcome;
use openlogi_device::fixture::{
    CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport,
};

use super::{RecordedChannel, RecordedChannelOpenOutcome, RecordedRequest, RecordedRequestFact};

mod sanitizer;

use sanitizer::{ProtocolSanitizer, unassociated_rejection};

#[cfg(test)]
mod tests;

/// Human-owned cassette fields that cannot be inferred from transport evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidCassetteMetadata {
    /// Human-readable operation name.
    pub name: String,
    /// Logical channel identifier used by the replay topology.
    pub channel: String,
}

/// A class of identity replaced by the protocol-aware sanitizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SanitizedIdentityKind {
    /// Bolt receiver unique ID from long register `0xfb`.
    ReceiverUniqueId,
    /// Unifying receiver serial from long register `0xb5/0x03`.
    ReceiverSerialNumber,
    /// Device unit ID from a receiver slot or HID++ 2.0 DeviceInformation.
    DeviceUnitId,
    /// Device serial from HID++ 2.0 DeviceInformation function `2`.
    DeviceSerialNumber,
}

/// One relation-preserving synthetic identity and its replacement count.
///
/// The original value is deliberately absent so audit output is safe to show
/// or serialize later. `synthetic_value` is generated sequentially, never by
/// hashing the source identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityReplacement {
    /// Protocol identity field that was replaced.
    pub kind: SanitizedIdentityKind,
    /// Length-preserving synthetic bytes written into the cassette.
    pub synthetic_value: Vec<u8>,
    /// Number of occurrences replaced with this value.
    pub occurrences: usize,
}

/// Privacy audit for a committable cassette candidate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HidCassetteAudit {
    /// Distinct relation-preserving identity replacements.
    pub replacements: Vec<IdentityReplacement>,
}

/// Why recorded evidence could not become a committable cassette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CassetteRejectionReason {
    /// The channel did not finish opening successfully.
    ChannelNotOpened,
    /// The opened channel exposes a report-width combination schema v1 cannot represent.
    UnsupportedReportSupport,
    /// The channel observer was still live when recording ended.
    IncompleteChannel,
    /// Two request records claimed the same channel request ID.
    DuplicateRequestId,
    /// A response-bearing request did not have exactly one outgoing report.
    OutgoingReportCount {
        /// Number of outgoing reports observed for the request.
        actual: usize,
    },
    /// A response-bearing request did not have exactly one terminal outcome.
    OutcomeCount {
        /// Number of terminal outcomes observed for the request.
        actual: usize,
    },
    /// A successful request did not have exactly one matched incoming report.
    IncomingReportCount {
        /// Number of matched incoming reports observed for the request.
        actual: usize,
    },
    /// The response-bearing request timed out.
    RequestTimedOut,
    /// The native transport rejected the request write.
    RequestWriteFailed,
    /// The request waiter disappeared without receiving a response.
    RequestLostResponse,
    /// The caller cancelled the in-flight request.
    RequestCancelled,
    /// A future request outcome is not classified by this builder.
    UnsupportedRequestOutcome,
    /// A fire-and-forget/raw channel write has no post-write success evidence.
    UnprovenFireAndForget,
    /// An incoming report was not matched to a response-bearing request.
    UnmatchedIncomingReport,
    /// Incoming bytes failed HID++ report parsing.
    MalformedIncomingReport,
    /// A future unassociated observation is not classified by this builder.
    UnsupportedObservation,
    /// A report has invalid or unsupported framing.
    MalformedReport,
    /// Recorded request and response headers do not correlate under their protocol.
    CorrelationMismatch,
    /// A HID++ 2.0 version ping received a HID++ 1.0 error response, which
    /// schema v1 cannot safely rebind to a different software ID.
    UnsupportedCrossVersionPing,
    /// HID++ 1.0 traffic is outside the supported receiver register layouts.
    UnsupportedHidpp10Register,
    /// A HID++ 2.0 runtime feature index was never learned from discovery traffic.
    UnknownFeatureIndex {
        /// Device index carried by the request.
        device_index: u8,
        /// Runtime feature index carried by the request.
        feature_index: u8,
    },
    /// A known feature can carry identity and has no safe sanitizer here.
    UnsupportedIdentityFeature {
        /// HID++ 2.0 feature ID.
        feature_id: u16,
    },
    /// A feature or function is outside the proven read-only classifier.
    UnsupportedHidpp20Function {
        /// HID++ 2.0 feature ID.
        feature_id: u16,
        /// HID++ 2.0 function ID.
        function_id: u8,
    },
    /// Feature discovery assigned one runtime index to conflicting feature IDs.
    AmbiguousFeatureMapping,
    /// An identity field did not satisfy its protocol-defined representation.
    MalformedIdentity,
    /// Sequential synthetic identifiers exhausted their fixed-width field.
    SyntheticIdentitySpaceExhausted,
    /// Pairing, discovery-address, or passkey traffic is never retained.
    PairingTraffic,
    /// No complete exchange remained after validation.
    EmptyCassette,
    /// The produced cassette violated its strict schema invariant.
    InvalidCassette {
        /// Validation diagnostic. It contains cassette metadata, never native node data.
        message: String,
    },
}

/// One typed rejection, optionally associated with a channel request ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CassetteRejection {
    /// Request ID that supplied the rejected evidence, when one exists.
    pub request_id: Option<u64>,
    /// Privacy, lifecycle, or protocol reason conversion was refused.
    pub reason: CassetteRejectionReason,
}

/// Result of auditing and attempting to build one cassette candidate.
///
/// `cassette` is present only when every piece of evidence was classified and
/// the strict fixture validator accepted the complete result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidCassetteBuildReport {
    /// Validated privacy-safe cassette, or `None` when any rejection occurred.
    pub cassette: Option<HidCassette>,
    /// Relation-preserving replacements made while classifying evidence.
    pub audit: HidCassetteAudit,
    /// Reasons the recording is not committable.
    pub rejections: Vec<CassetteRejection>,
}

impl HidCassetteBuildReport {
    /// Whether the report contains one validated cassette and no rejection.
    #[must_use]
    pub fn is_committable(&self) -> bool {
        self.cassette.is_some() && self.rejections.is_empty()
    }
}

impl RecordedChannel {
    /// Audit and convert this explicitly selected completed channel lifetime.
    ///
    /// Requests are associated only by their recorder-local request IDs. The
    /// resulting exchanges are ordered by those monotonically assigned IDs, so
    /// repeated normalized request keys retain deterministic FIFO responses
    /// without blessing callback scheduling as a global replay order.
    #[must_use]
    pub fn build_hid_cassette(&self, metadata: HidCassetteMetadata) -> HidCassetteBuildReport {
        CassetteBuilder::new(self, metadata).build()
    }
}

struct CassetteBuilder<'a> {
    channel: &'a RecordedChannel,
    metadata: HidCassetteMetadata,
    sanitizer: ProtocolSanitizer,
    rejections: Vec<CassetteRejection>,
}

impl<'a> CassetteBuilder<'a> {
    fn new(channel: &'a RecordedChannel, metadata: HidCassetteMetadata) -> Self {
        Self {
            channel,
            metadata,
            sanitizer: ProtocolSanitizer::default(),
            rejections: Vec::new(),
        }
    }

    fn build(mut self) -> HidCassetteBuildReport {
        let report_support = self.validate_channel();
        self.validate_unassociated();

        let mut requests: Vec<_> = self.channel.requests.iter().collect();
        requests.sort_by_key(|request| request.request_id);
        self.reject_duplicate_request_ids(&requests);

        let mut exchanges = Vec::with_capacity(requests.len());
        for request in requests {
            if let Some(exchange) = self.build_exchange(request) {
                exchanges.push(exchange);
            }
        }

        if exchanges.is_empty() {
            self.reject(None, CassetteRejectionReason::EmptyCassette);
        }

        let cassette = if self.rejections.is_empty() {
            if let Some(report_support) = report_support {
                let cassette = HidCassette {
                    schema_version: FIXTURE_SCHEMA_VERSION,
                    name: self.metadata.name.clone(),
                    channel: self.metadata.channel.clone(),
                    report_support,
                    exchanges,
                };
                match cassette.validate() {
                    Ok(()) => Some(cassette),
                    Err(error) => {
                        self.reject(
                            None,
                            CassetteRejectionReason::InvalidCassette {
                                message: error.to_string(),
                            },
                        );
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        let audit = self.sanitizer.finish();

        HidCassetteBuildReport {
            cassette,
            audit,
            rejections: self.rejections,
        }
    }

    fn validate_channel(&mut self) -> Option<ReportSupport> {
        let support = match self.channel.open_outcome {
            RecordedChannelOpenOutcome::Opened {
                supports_short: true,
                supports_long: true,
            } => Some(ReportSupport::ShortAndLong),
            RecordedChannelOpenOutcome::Opened {
                supports_short: false,
                supports_long: true,
            } => Some(ReportSupport::LongOnly),
            RecordedChannelOpenOutcome::Opened { .. } => {
                self.reject(None, CassetteRejectionReason::UnsupportedReportSupport);
                None
            }
            RecordedChannelOpenOutcome::NotHidpp
            | RecordedChannelOpenOutcome::Failed(_)
            | RecordedChannelOpenOutcome::Cancelled => {
                self.reject(None, CassetteRejectionReason::ChannelNotOpened);
                None
            }
        };
        if self.channel.closed_at.is_none() {
            self.reject(None, CassetteRejectionReason::IncompleteChannel);
        }
        support
    }

    fn validate_unassociated(&mut self) {
        for evidence in &self.channel.unassociated {
            self.reject(None, unassociated_rejection(&evidence.observation));
        }
    }

    fn reject_duplicate_request_ids(&mut self, requests: &[&RecordedRequest]) {
        let mut ids = BTreeSet::new();
        for request in requests {
            if !ids.insert(request.request_id) {
                self.reject(
                    Some(request.request_id),
                    CassetteRejectionReason::DuplicateRequestId,
                );
            }
        }
    }

    fn build_exchange(&mut self, request: &RecordedRequest) -> Option<CassetteExchange> {
        let mut outgoing = Vec::new();
        let mut incoming = Vec::new();
        let mut outcomes = Vec::new();
        for fact in &request.facts {
            match fact {
                RecordedRequestFact::OutgoingReport { report, .. } => outgoing.push(report),
                RecordedRequestFact::IncomingReport { report, .. } => incoming.push(report),
                RecordedRequestFact::Outcome { outcome, .. } => outcomes.push(*outcome),
            }
        }

        let mut valid = true;
        if outgoing.len() != 1 {
            self.reject(
                Some(request.request_id),
                CassetteRejectionReason::OutgoingReportCount {
                    actual: outgoing.len(),
                },
            );
            valid = false;
        }
        if outcomes.len() != 1 {
            self.reject(
                Some(request.request_id),
                CassetteRejectionReason::OutcomeCount {
                    actual: outcomes.len(),
                },
            );
            valid = false;
        }
        if !valid {
            return None;
        }

        match outcomes[0] {
            RequestOutcome::Succeeded => {}
            RequestOutcome::TimedOut => {
                self.reject(
                    Some(request.request_id),
                    CassetteRejectionReason::RequestTimedOut,
                );
                return None;
            }
            RequestOutcome::WriteFailed => {
                self.reject(
                    Some(request.request_id),
                    CassetteRejectionReason::RequestWriteFailed,
                );
                return None;
            }
            RequestOutcome::NoResponse => {
                self.reject(
                    Some(request.request_id),
                    CassetteRejectionReason::RequestLostResponse,
                );
                return None;
            }
            RequestOutcome::Cancelled => {
                self.reject(
                    Some(request.request_id),
                    CassetteRejectionReason::RequestCancelled,
                );
                return None;
            }
            _ => {
                self.reject(
                    Some(request.request_id),
                    CassetteRejectionReason::UnsupportedRequestOutcome,
                );
                return None;
            }
        }

        if incoming.len() != 1 {
            self.reject(
                Some(request.request_id),
                CassetteRejectionReason::IncomingReportCount {
                    actual: incoming.len(),
                },
            );
            return None;
        }

        match self
            .sanitizer
            .exchange(outgoing[0].as_bytes(), incoming[0].as_bytes())
        {
            Ok(exchange) => Some(exchange),
            Err(reason) => {
                self.reject(Some(request.request_id), reason);
                None
            }
        }
    }

    fn reject(&mut self, request_id: Option<u64>, reason: CassetteRejectionReason) {
        self.rejections
            .push(CassetteRejection { request_id, reason });
    }
}
