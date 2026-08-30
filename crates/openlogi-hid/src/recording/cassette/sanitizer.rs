use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use hidpp::channel::ChannelObservation;
use openlogi_device::fixture::{CassetteExchange, RequestMatch};

use super::{
    CassetteRejectionReason, HidCassetteAudit, IdentityReplacement, SanitizedIdentityKind,
};

const ROOT: u16 = 0x0000;
const FEATURE_SET: u16 = 0x0001;
const DEVICE_INFORMATION: u16 = 0x0003;
const DEVICE_TYPE_AND_NAME: u16 = 0x0005;

enum Hidpp10Operation {
    ReceiverControl,
    ReceiverUniqueId,
    ReceiverSerialNumber,
    DeviceUnitId,
    UnifyingCodename,
    BoltCodename,
}

#[derive(Default)]
pub(super) struct ProtocolSanitizer {
    features: BTreeMap<(u8, u8), u16>,
    identities: IdentitySanitizer,
}

impl ProtocolSanitizer {
    pub(super) fn exchange(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<CassetteExchange, CassetteRejectionReason> {
        validate_report(request)?;
        validate_report(response)?;
        let hidpp10 = self.is_hidpp10_request(request);
        if hidpp10 && (is_pairing_report(request) || is_pairing_report(response)) {
            return Err(CassetteRejectionReason::PairingTraffic);
        }

        let mut request = request.to_vec();
        let mut response = response.to_vec();
        let request_match = if hidpp10 {
            self.sanitize_hidpp10(&request, &mut response)?;
            RequestMatch::Exact
        } else {
            self.sanitize_hidpp20(&request, &mut response)?;
            normalize_hidpp20(&mut request, &mut response);
            RequestMatch::Hidpp20
        };

        Ok(CassetteExchange {
            request_match,
            request,
            response: Some(response),
            required: true,
        })
    }

    pub(super) fn finish(self) -> HidCassetteAudit {
        self.identities.finish()
    }

    fn sanitize_hidpp10(
        &mut self,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<(), CassetteRejectionReason> {
        validate_hidpp10_correlation(request, response)?;
        let operation = classify_hidpp10(request)?;
        if is_hidpp10_error(response) {
            require_short(response)?;
            return Ok(());
        }

        match operation {
            Hidpp10Operation::ReceiverControl => require_short(response),
            Hidpp10Operation::ReceiverUniqueId => {
                require_long(response)?;
                if str::from_utf8(&response[4..20]).is_err() {
                    return Err(CassetteRejectionReason::MalformedIdentity);
                }
                self.identities
                    .replace(SanitizedIdentityKind::ReceiverUniqueId, response, 4..20)
            }
            Hidpp10Operation::ReceiverSerialNumber => {
                validate_receiver_info_response(request, response)?;
                // Unifying receiver info response `[sub, serial:4, _, slots, ..]`.
                self.identities
                    .replace(SanitizedIdentityKind::ReceiverSerialNumber, response, 5..9)
            }
            Hidpp10Operation::DeviceUnitId => {
                validate_receiver_info_response(request, response)?;
                // Pairing info response `[sub, flags, wpid:2, unit_id:4, ..]`.
                self.identities
                    .replace(SanitizedIdentityKind::DeviceUnitId, response, 8..12)
            }
            // These receiver-stored codenames are model data, not user-assigned
            // names. Validate the protocol-specific layout before retaining it.
            Hidpp10Operation::UnifyingCodename => {
                validate_receiver_info_response(request, response)?;
                validate_utf8_field(response, 6, response[5], 20)
            }
            Hidpp10Operation::BoltCodename => {
                validate_receiver_info_response(request, response)?;
                validate_utf8_field(response, 7, response[6], 20)
            }
        }
    }

    fn sanitize_hidpp20(
        &mut self,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<(), CassetteRejectionReason> {
        if is_cross_version_ping(request, response) {
            return Err(CassetteRejectionReason::UnsupportedCrossVersionPing);
        }
        validate_hidpp20_correlation(request, response)?;
        let device_index = request[1];
        let feature_index = request[2];
        let function_id = request[3] >> 4;
        let feature_id = if feature_index == 0 {
            ROOT
        } else {
            *self.features.get(&(device_index, feature_index)).ok_or(
                CassetteRejectionReason::UnknownFeatureIndex {
                    device_index,
                    feature_index,
                },
            )?
        };

        if is_identity_feature(feature_id) {
            return Err(CassetteRejectionReason::UnsupportedIdentityFeature { feature_id });
        }
        if is_hidpp20_error(response) {
            Self::validate_supported_function(feature_id, function_id)?;
            require_short(response)?;
            return Ok(());
        }

        match (feature_id, function_id) {
            (ROOT, 0) => self.learn_root_mapping(request, response),
            (ROOT, 1) | (FEATURE_SET, 0) | (DEVICE_TYPE_AND_NAME, 0..=2) => Ok(()),
            (FEATURE_SET, 1) => self.learn_feature_set_mapping(request, response),
            (DEVICE_INFORMATION, 0) => {
                require_long(response)?;
                self.identities
                    .replace(SanitizedIdentityKind::DeviceUnitId, response, 5..9)
            }
            (DEVICE_INFORMATION, 1) => {
                require_long(response)?;
                Ok(())
            }
            (DEVICE_INFORMATION, 2) => {
                require_long(response)?;
                if str::from_utf8(&response[4..16]).is_err() {
                    return Err(CassetteRejectionReason::MalformedIdentity);
                }
                self.identities
                    .replace(SanitizedIdentityKind::DeviceSerialNumber, response, 4..16)
            }
            _ => Self::validate_supported_function(feature_id, function_id),
        }
    }

    fn learn_root_mapping(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<(), CassetteRejectionReason> {
        let feature_id = u16::from_be_bytes([request[4], request[5]]);
        let feature_index = response[4];
        if feature_index == 0 {
            return Ok(());
        }
        self.insert_feature(request[1], feature_index, feature_id)
    }

    fn learn_feature_set_mapping(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<(), CassetteRejectionReason> {
        let feature_index = request[4];
        let feature_id = u16::from_be_bytes([response[4], response[5]]);
        if feature_index == 0 {
            return if feature_id == ROOT {
                Ok(())
            } else {
                Err(CassetteRejectionReason::MalformedReport)
            };
        }
        self.insert_feature(request[1], feature_index, feature_id)
    }

    fn insert_feature(
        &mut self,
        device_index: u8,
        feature_index: u8,
        feature_id: u16,
    ) -> Result<(), CassetteRejectionReason> {
        match self
            .features
            .insert((device_index, feature_index), feature_id)
        {
            Some(previous) if previous != feature_id => {
                Err(CassetteRejectionReason::AmbiguousFeatureMapping)
            }
            _ => Ok(()),
        }
    }

    fn is_hidpp10_request(&self, report: &[u8]) -> bool {
        report[1] == 0xff
            && matches!(report[2], 0x80..=0x83)
            && !self.features.contains_key(&(report[1], report[2]))
    }

    fn validate_supported_function(
        feature_id: u16,
        function_id: u8,
    ) -> Result<(), CassetteRejectionReason> {
        let supported = match feature_id {
            ROOT | FEATURE_SET | 0x1004 | 0x2111 | 0x2150 => matches!(function_id, 0..=1),
            DEVICE_INFORMATION | DEVICE_TYPE_AND_NAME | 0x2201 => {
                matches!(function_id, 0..=2)
            }
            0x1000 | 0x1001 | 0x2100 | 0x2110 => function_id == 0,
            0x1982 => matches!(function_id, 0 | 2),
            0x1b04 => matches!(function_id, 0..=2 | 4),
            0x2121 => matches!(function_id, 0..=1 | 3),
            0x2202 => matches!(function_id, 0..=5 | 8),
            0x6501 => matches!(function_id, 0 | 3),
            _ => false,
        };
        if supported {
            Ok(())
        } else {
            Err(CassetteRejectionReason::UnsupportedHidpp20Function {
                feature_id,
                function_id,
            })
        }
    }
}

pub(super) fn unassociated_rejection(observation: &ChannelObservation) -> CassetteRejectionReason {
    match observation {
        ChannelObservation::OutgoingReport { report, .. } => {
            if is_pairing_report(report.as_bytes()) {
                CassetteRejectionReason::PairingTraffic
            } else {
                CassetteRejectionReason::UnprovenFireAndForget
            }
        }
        ChannelObservation::IncomingReport { report, .. } => {
            if is_pairing_report(report.as_bytes()) {
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

fn validate_report(report: &[u8]) -> Result<(), CassetteRejectionReason> {
    let valid = matches!(
        (report.first(), report.len()),
        (Some(0x10), 7) | (Some(0x11), 20)
    );
    if valid {
        Ok(())
    } else {
        Err(CassetteRejectionReason::MalformedReport)
    }
}

fn classify_hidpp10(request: &[u8]) -> Result<Hidpp10Operation, CassetteRejectionReason> {
    require_short(request)?;
    match (request[2], request[3], request[4]) {
        // Read/write notification state and read/write connection count or
        // arrival trigger. These receiver control fields carry no identity.
        (0x80 | 0x81, 0x00 | 0x02, _) => Ok(Hidpp10Operation::ReceiverControl),
        (0x83, 0xfb, _) => Ok(Hidpp10Operation::ReceiverUniqueId),
        (0x83, 0xb5, 0x03) => Ok(Hidpp10Operation::ReceiverSerialNumber),
        (0x83, 0xb5, 0x51..=0x56) => Ok(Hidpp10Operation::DeviceUnitId),
        (0x83, 0xb5, 0x40..=0x45) => Ok(Hidpp10Operation::UnifyingCodename),
        (0x83, 0xb5, 0x61..=0x66) => Ok(Hidpp10Operation::BoltCodename),
        _ => Err(CassetteRejectionReason::UnsupportedHidpp10Register),
    }
}

fn validate_receiver_info_response(
    request: &[u8],
    response: &[u8],
) -> Result<(), CassetteRejectionReason> {
    require_long(response)?;
    if response[4] == request[4] {
        Ok(())
    } else {
        Err(CassetteRejectionReason::CorrelationMismatch)
    }
}

fn validate_hidpp10_correlation(
    request: &[u8],
    response: &[u8],
) -> Result<(), CassetteRejectionReason> {
    let normal =
        response[1] == request[1] && response[2] == request[2] && response[3] == request[3];
    let error = response[1] == request[1]
        && response[2] == 0x8f
        && response[3] == request[2]
        && response[4] == request[3];
    if normal || error {
        Ok(())
    } else {
        Err(CassetteRejectionReason::CorrelationMismatch)
    }
}

fn validate_hidpp20_correlation(
    request: &[u8],
    response: &[u8],
) -> Result<(), CassetteRejectionReason> {
    let normal =
        response[1] == request[1] && response[2] == request[2] && response[3] == request[3];
    let error = response[1] == request[1]
        && response[2] == 0xff
        && response[3] == request[2]
        && response[4] == request[3];
    if normal || error {
        Ok(())
    } else {
        Err(CassetteRejectionReason::CorrelationMismatch)
    }
}

fn is_cross_version_ping(request: &[u8], response: &[u8]) -> bool {
    request[0] == 0x10
        && request[2] == 0
        && request[3] >> 4 == 1
        && response[0] == 0x10
        && response[1] == request[1]
        && response[2] == 0x8f
        && response[3] == request[2]
        && response[4] == request[3]
}

fn is_hidpp10_error(response: &[u8]) -> bool {
    response[2] == 0x8f
}

fn is_hidpp20_error(response: &[u8]) -> bool {
    response[2] == 0xff
}

fn normalize_hidpp20(request: &mut [u8], response: &mut [u8]) {
    request[3] &= 0xf0;
    if is_hidpp20_error(response) {
        response[4] &= 0xf0;
    } else {
        response[3] &= 0xf0;
    }
}

fn is_identity_feature(feature_id: u16) -> bool {
    matches!(feature_id, 0x0004 | 0x0007 | 0x0021 | 0x1814 | 0x1815)
}

fn is_pairing_report(report: &[u8]) -> bool {
    if report.len() < 4 {
        return false;
    }
    let receiver_notification =
        report[1] == 0xff && matches!(report[2], 0x4a | 0x4d | 0x4e | 0x4f | 0x53 | 0x54);
    let receiver_register = report[1] == 0xff
        && matches!(report[2], 0x80..=0x83)
        && matches!(report[3], 0xb2 | 0xc0 | 0xc1);
    let receiver_register_error = report[1] == 0xff
        && report[2] == 0x8f
        && matches!(report[3], 0x80..=0x83)
        && report
            .get(4)
            .is_some_and(|address| matches!(address, 0xb2 | 0xc0 | 0xc1));
    receiver_notification || receiver_register || receiver_register_error
}

fn require_long(report: &[u8]) -> Result<(), CassetteRejectionReason> {
    if report[0] == 0x11 {
        Ok(())
    } else {
        Err(CassetteRejectionReason::MalformedIdentity)
    }
}

fn require_short(report: &[u8]) -> Result<(), CassetteRejectionReason> {
    if report[0] == 0x10 {
        Ok(())
    } else {
        Err(CassetteRejectionReason::MalformedReport)
    }
}

fn validate_utf8_field(
    response: &[u8],
    start: usize,
    length: u8,
    limit: usize,
) -> Result<(), CassetteRejectionReason> {
    let end = start
        .checked_add(usize::from(length))
        .filter(|end| *end <= limit)
        .ok_or(CassetteRejectionReason::MalformedReport)?;
    str::from_utf8(&response[start..end])
        .map(|_| ())
        .map_err(|_| CassetteRejectionReason::MalformedReport)
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
    counters: BTreeMap<SanitizedIdentityKind, u32>,
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
            let candidate = synthetic_value(kind, *counter)?;
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

fn synthetic_value(
    kind: SanitizedIdentityKind,
    counter: u32,
) -> Result<Vec<u8>, CassetteRejectionReason> {
    match kind {
        SanitizedIdentityKind::ReceiverUniqueId => Ok(format!("{counter:016X}").into_bytes()),
        SanitizedIdentityKind::DeviceSerialNumber => Ok(format!("{counter:012X}").into_bytes()),
        SanitizedIdentityKind::ReceiverSerialNumber => tagged_u32(0xa000_0000, counter)
            .map(u32::to_be_bytes)
            .map(Vec::from),
        SanitizedIdentityKind::DeviceUnitId => tagged_u32(0xd000_0000, counter)
            .map(u32::to_be_bytes)
            .map(Vec::from),
    }
}

fn tagged_u32(tag: u32, counter: u32) -> Result<u32, CassetteRejectionReason> {
    if counter > 0x0fff_ffff {
        Err(CassetteRejectionReason::SyntheticIdentitySpaceExhausted)
    } else {
        Ok(tag | counter)
    }
}
