//! Privacy-safe rendering and cassette construction from native evidence.

use anyhow::{Result, bail};
use openlogi_hid::recording::{
    CassetteRejectionReason, HidCassetteBuildReport, HidCassetteMetadata, NativeRecording,
    SanitizedIdentityKind,
};

use super::{SanitizedCandidate, uppercase_hex};

pub(super) fn sanitize_recording(
    recording: NativeRecording,
    name: &str,
    channel: &str,
) -> Result<Vec<SanitizedCandidate>> {
    let recorded_raw_writers = !recording.raw_writers.is_empty();
    let mut candidates = Vec::new();
    for (index, recorded_channel) in recording.channels.iter().enumerate() {
        let report = recorded_channel.build_hid_cassette(HidCassetteMetadata {
            name: name.to_string(),
            channel: channel.to_string(),
        });
        render_build_report(index, &report);
        if report.rejections.is_empty()
            && let Some(cassette) = report.cassette.clone()
        {
            candidates.push(SanitizedCandidate {
                cassette,
                audit: report.audit,
            });
        }
    }
    drop(recording);

    if recorded_raw_writers {
        bail!(
            "read-only HID++ case capture unexpectedly opened a raw writer; no fixture was written"
        );
    }
    Ok(candidates)
}

fn render_build_report(index: usize, report: &HidCassetteBuildReport) {
    if report.is_committable() {
        eprintln!("channel candidate {}: sanitizer accepted", index + 1);
    } else {
        eprintln!("channel candidate {}: sanitizer rejected", index + 1);
    }
    for replacement in &report.audit.replacements {
        eprintln!(
            "  audit: {} replaced {} occurrence(s) with synthetic {}",
            identity_kind_label(replacement.kind),
            replacement.occurrences,
            uppercase_hex(&replacement.synthetic_value)
        );
    }
    for rejection in &report.rejections {
        eprintln!("  rejection: {}", rejection_label(&rejection.reason));
    }
}

fn identity_kind_label(kind: SanitizedIdentityKind) -> &'static str {
    match kind {
        SanitizedIdentityKind::ReceiverUniqueId => "Bolt receiver identity",
        SanitizedIdentityKind::ReceiverSerialNumber => "Unifying receiver identity",
        SanitizedIdentityKind::DeviceUnitId => "device unit identity",
        SanitizedIdentityKind::DeviceSerialNumber => "device serial identity",
    }
}

fn rejection_label(reason: &CassetteRejectionReason) -> String {
    match reason {
        CassetteRejectionReason::ChannelNotOpened => "channel did not open".to_string(),
        CassetteRejectionReason::UnsupportedReportSupport => {
            "unsupported HID++ report-width combination".to_string()
        }
        CassetteRejectionReason::IncompleteChannel => "channel evidence is incomplete".to_string(),
        CassetteRejectionReason::DuplicateRequestId => {
            "duplicate recorder request identifier".to_string()
        }
        CassetteRejectionReason::OutgoingReportCount { actual } => {
            format!("request has {actual} outgoing reports instead of one")
        }
        CassetteRejectionReason::OutcomeCount { actual } => {
            format!("request has {actual} terminal outcomes instead of one")
        }
        CassetteRejectionReason::IncomingReportCount { actual } => {
            format!("successful request has {actual} responses instead of one")
        }
        CassetteRejectionReason::RequestTimedOut => "request timed out".to_string(),
        CassetteRejectionReason::RequestWriteFailed => "request write failed".to_string(),
        CassetteRejectionReason::RequestLostResponse => "request lost its response".to_string(),
        CassetteRejectionReason::RequestCancelled => "request was cancelled".to_string(),
        CassetteRejectionReason::UnsupportedRequestOutcome => {
            "request outcome is not classified".to_string()
        }
        CassetteRejectionReason::UnprovenFireAndForget => {
            "fire-and-forget write has no success evidence".to_string()
        }
        CassetteRejectionReason::UnmatchedIncomingReport => {
            "incoming report was not associated with a request".to_string()
        }
        CassetteRejectionReason::MalformedIncomingReport => {
            "incoming report was malformed".to_string()
        }
        CassetteRejectionReason::UnsupportedObservation => {
            "channel observation is not classified".to_string()
        }
        CassetteRejectionReason::MalformedReport => "report framing is malformed".to_string(),
        CassetteRejectionReason::CorrelationMismatch => {
            "request and response do not correlate".to_string()
        }
        CassetteRejectionReason::UnsupportedCrossVersionPing => {
            "cross-version HID++ ping cannot be safely rebound".to_string()
        }
        CassetteRejectionReason::UnsupportedHidpp10Register => {
            "HID++ 1.0 register is outside the read-only allowlist".to_string()
        }
        CassetteRejectionReason::UnknownFeatureIndex {
            device_index,
            feature_index,
        } => format!(
            "runtime feature index {feature_index:#04x} on device {device_index:#04x} is unknown"
        ),
        CassetteRejectionReason::UnsupportedIdentityFeature { feature_id } => {
            format!("identity-bearing feature {feature_id:#06x} has no safe sanitizer")
        }
        CassetteRejectionReason::UnsupportedHidpp20Function {
            feature_id,
            function_id,
        } => format!(
            "feature {feature_id:#06x} function {function_id:#04x} is outside the read-only allowlist"
        ),
        CassetteRejectionReason::AmbiguousFeatureMapping => {
            "runtime feature mapping is ambiguous".to_string()
        }
        CassetteRejectionReason::MalformedIdentity => {
            "identity field has an invalid protocol representation".to_string()
        }
        CassetteRejectionReason::SyntheticIdentitySpaceExhausted => {
            "synthetic identity space is exhausted".to_string()
        }
        CassetteRejectionReason::PairingTraffic => {
            "pairing, discovery-address, or passkey traffic is forbidden".to_string()
        }
        CassetteRejectionReason::EmptyCassette => {
            "no complete exchange remained after validation".to_string()
        }
        CassetteRejectionReason::InvalidCassette { .. } => {
            "sanitized candidate failed strict cassette validation".to_string()
        }
    }
}
