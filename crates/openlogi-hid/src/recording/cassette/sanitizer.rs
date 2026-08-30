use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use hidpp::channel::ChannelObservation;
use openlogi_device::fixture::{
    CassetteExchange, ProtocolIdentityError, ProtocolIdentityExtractor, RequestMatch,
    SyntheticIdentityKind, SyntheticIdentityOrdinal, generate_synthetic_identity,
    is_pairing_identity_traffic,
};

use super::{
    CassetteRejectionReason, HidCassetteAudit, IdentityReplacement, SanitizedIdentityKind,
};

#[derive(Default)]
pub(super) struct ProtocolSanitizer {
    protocol: ProtocolIdentityExtractor,
    identities: IdentitySanitizer,
}

impl ProtocolSanitizer {
    pub(super) fn exchange(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<CassetteExchange, CassetteRejectionReason> {
        let inspection = self
            .protocol
            .inspect_exchange(request, response)
            .map_err(|error| map_protocol_error(&error))?;
        let mut request = request.to_vec();
        let mut response = response.to_vec();
        for field in inspection.fields {
            let kind = SanitizedIdentityKind::from_policy(field.kind)
                .ok_or(CassetteRejectionReason::MalformedIdentity)?;
            self.identities.replace(kind, &mut response, field.range)?;
        }
        if inspection.request_match == RequestMatch::Hidpp20 {
            normalize_hidpp20(&mut request, &mut response);
        }
        Ok(CassetteExchange {
            request_match: inspection.request_match,
            request,
            response: Some(response),
            required: true,
        })
    }

    pub(super) fn finish(self) -> HidCassetteAudit {
        self.identities.finish()
    }
}

pub(super) fn unassociated_rejection(observation: &ChannelObservation) -> CassetteRejectionReason {
    match observation {
        ChannelObservation::OutgoingReport { report, .. } => {
            if is_pairing_identity_traffic(report.as_bytes()) {
                CassetteRejectionReason::PairingTraffic
            } else {
                CassetteRejectionReason::UnprovenFireAndForget
            }
        }
        ChannelObservation::IncomingReport { report, .. } => {
            if is_pairing_identity_traffic(report.as_bytes()) {
                CassetteRejectionReason::PairingTraffic
            } else {
                CassetteRejectionReason::UnmatchedIncomingReport
            }
        }
        ChannelObservation::MalformedIncomingReport { .. } => {
            CassetteRejectionReason::MalformedIncomingReport
        }
        ChannelObservation::RequestOutcome { .. } => {
            CassetteRejectionReason::UnsupportedObservation
        }
        _ => CassetteRejectionReason::UnsupportedObservation,
    }
}

fn map_protocol_error(error: &ProtocolIdentityError) -> CassetteRejectionReason {
    match error {
        ProtocolIdentityError::MalformedReport => CassetteRejectionReason::MalformedReport,
        ProtocolIdentityError::CorrelationMismatch => CassetteRejectionReason::CorrelationMismatch,
        ProtocolIdentityError::UnsupportedCrossVersionPing => {
            CassetteRejectionReason::UnsupportedCrossVersionPing
        }
        ProtocolIdentityError::UnsupportedHidpp10Register => {
            CassetteRejectionReason::UnsupportedHidpp10Register
        }
        ProtocolIdentityError::UnknownFeatureIndex {
            device_index,
            feature_index,
        } => CassetteRejectionReason::UnknownFeatureIndex {
            device_index: *device_index,
            feature_index: *feature_index,
        },
        ProtocolIdentityError::UnsupportedIdentityFeature { feature_id } => {
            CassetteRejectionReason::UnsupportedIdentityFeature {
                feature_id: *feature_id,
            }
        }
        ProtocolIdentityError::UnsupportedHidpp20Function {
            feature_id,
            function_id,
        } => CassetteRejectionReason::UnsupportedHidpp20Function {
            feature_id: *feature_id,
            function_id: *function_id,
        },
        ProtocolIdentityError::AmbiguousFeatureMapping => {
            CassetteRejectionReason::AmbiguousFeatureMapping
        }
        ProtocolIdentityError::MalformedIdentity
        | ProtocolIdentityError::NonSyntheticIdentity { .. } => {
            CassetteRejectionReason::MalformedIdentity
        }
        ProtocolIdentityError::PairingTraffic => CassetteRejectionReason::PairingTraffic,
    }
}

fn normalize_hidpp20(request: &mut [u8], response: &mut [u8]) {
    request[3] &= 0xf0;
    if response[2] == 0xff {
        response[4] &= 0xf0;
    } else {
        response[3] &= 0xf0;
    }
}

impl SanitizedIdentityKind {
    const fn from_policy(kind: SyntheticIdentityKind) -> Option<Self> {
        match kind {
            SyntheticIdentityKind::BoltReceiverUid => Some(Self::ReceiverUniqueId),
            SyntheticIdentityKind::UnifyingReceiverSerial => Some(Self::ReceiverSerialNumber),
            SyntheticIdentityKind::DeviceUnitId => Some(Self::DeviceUnitId),
            SyntheticIdentityKind::DeviceSerialNumber => Some(Self::DeviceSerialNumber),
            SyntheticIdentityKind::UnifyingReceiverRoute
            | SyntheticIdentityKind::RawHidProfileIdentity => None,
        }
    }

    const fn policy_kind(self) -> SyntheticIdentityKind {
        match self {
            Self::ReceiverUniqueId => SyntheticIdentityKind::BoltReceiverUid,
            Self::ReceiverSerialNumber => SyntheticIdentityKind::UnifyingReceiverSerial,
            Self::DeviceUnitId => SyntheticIdentityKind::DeviceUnitId,
            Self::DeviceSerialNumber => SyntheticIdentityKind::DeviceSerialNumber,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct IdentityKey {
    kind: SanitizedIdentityKind,
    original: Vec<u8>,
}

#[derive(Clone, Debug)]
struct ReplacementState {
    synthetic: Vec<u8>,
    occurrences: usize,
}

#[derive(Default)]
struct IdentitySanitizer {
    replacements: BTreeMap<IdentityKey, ReplacementState>,
    used: BTreeSet<(SanitizedIdentityKind, Vec<u8>)>,
    counters: BTreeMap<SanitizedIdentityKind, u16>,
}

impl IdentitySanitizer {
    fn replace(
        &mut self,
        kind: SanitizedIdentityKind,
        response: &mut [u8],
        range: Range<usize>,
    ) -> Result<(), CassetteRejectionReason> {
        let original = response
            .get(range.clone())
            .ok_or(CassetteRejectionReason::MalformedIdentity)?
            .to_vec();
        if original.iter().all(|byte| *byte == 0) {
            return Ok(());
        }

        let key = IdentityKey { kind, original };
        if let Some(replacement) = self.replacements.get_mut(&key) {
            replacement.occurrences = replacement.occurrences.saturating_add(1);
            response[range].copy_from_slice(&replacement.synthetic);
            return Ok(());
        }

        let synthetic = self.next_synthetic(kind, &key.original)?;
        response[range].copy_from_slice(&synthetic);
        self.used.insert((kind, synthetic.clone()));
        self.replacements.insert(
            key,
            ReplacementState {
                synthetic,
                occurrences: 1,
            },
        );
        Ok(())
    }

    fn next_synthetic(
        &mut self,
        kind: SanitizedIdentityKind,
        original: &[u8],
    ) -> Result<Vec<u8>, CassetteRejectionReason> {
        loop {
            let counter = self.counters.entry(kind).or_default();
            *counter = counter
                .checked_add(1)
                .ok_or(CassetteRejectionReason::SyntheticIdentitySpaceExhausted)?;
            let ordinal = SyntheticIdentityOrdinal::new(*counter)
                .map_err(|_| CassetteRejectionReason::SyntheticIdentitySpaceExhausted)?;
            let generated = generate_synthetic_identity(kind.policy_kind(), ordinal);
            let candidate = generated
                .as_bytes()
                .ok_or(CassetteRejectionReason::SyntheticIdentitySpaceExhausted)?
                .to_vec();
            // Capture always replaces the original. If an original already
            // resembles policy output, consume another ordinal rather than
            // treating classifier acceptance as proof of sanitization.
            if candidate != original && !self.used.contains(&(kind, candidate.clone())) {
                return Ok(candidate);
            }
        }
    }

    fn finish(self) -> HidCassetteAudit {
        let mut replacements: Vec<_> = self
            .replacements
            .into_iter()
            .map(|(key, state)| IdentityReplacement {
                kind: key.kind,
                synthetic_value: state.synthetic,
                occurrences: state.occurrences,
            })
            .collect();
        replacements.sort_by(|left, right| {
            (left.kind, &left.synthetic_value).cmp(&(right.kind, &right.synthetic_value))
        });
        HidCassetteAudit { replacements }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_replaces_even_an_original_that_matches_the_policy() {
        let mut sanitizer = IdentitySanitizer::default();
        let mut response = b"OL-BOLT-UID-0001".to_vec();
        sanitizer
            .replace(
                SanitizedIdentityKind::ReceiverUniqueId,
                &mut response,
                0..16,
            )
            .expect("policy-looking original is still replaceable");
        assert_eq!(response, b"OL-BOLT-UID-0002");
        let audit = sanitizer.finish();
        assert_eq!(audit.replacements.len(), 1);
        assert_eq!(audit.replacements[0].occurrences, 1);
    }

    #[test]
    fn sanitizer_reports_policy_ordinal_exhaustion() {
        let mut sanitizer = IdentitySanitizer::default();
        for ordinal in 1..=u8::MAX {
            let candidate = sanitizer
                .next_synthetic(SanitizedIdentityKind::DeviceUnitId, &[0; 4])
                .expect("bounded policy ordinal remains available");
            assert_eq!(candidate, [b'O', b'L', b'D', ordinal]);
        }
        assert_eq!(
            sanitizer.next_synthetic(SanitizedIdentityKind::DeviceUnitId, &[0; 4]),
            Err(CassetteRejectionReason::SyntheticIdentitySpaceExhausted)
        );
    }
}
