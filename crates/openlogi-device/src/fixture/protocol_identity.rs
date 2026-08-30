use std::collections::BTreeMap;
use std::ops::Range;

use thiserror::Error;

use super::{
    RequestMatch, SyntheticIdentityError, SyntheticIdentityKind, SyntheticIdentityOrdinal,
    classify_synthetic_identity_bytes,
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

/// One identity-bearing response field located by the protocol extractor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolIdentityField {
    /// Strong representation tag for these bytes.
    pub kind: SyntheticIdentityKind,
    /// Byte range within the response report.
    pub range: Range<usize>,
}

impl ProtocolIdentityField {
    /// Borrow this field's bytes from the inspected response.
    pub fn value<'a>(&self, response: &'a [u8]) -> Result<&'a [u8], ProtocolIdentityError> {
        response
            .get(self.range.clone())
            .ok_or(ProtocolIdentityError::MalformedIdentity)
    }

    /// Classify this field as canonical synthetic evidence.
    pub fn classify(
        &self,
        response: &[u8],
    ) -> Result<SyntheticIdentityOrdinal, ProtocolIdentityError> {
        classify_synthetic_identity_bytes(self.kind, self.value(response)?).map_err(|source| {
            ProtocolIdentityError::NonSyntheticIdentity {
                kind: self.kind,
                source,
            }
        })
    }
}

/// Read-only classification of one request/response exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolExchangeIdentity {
    /// Cassette request matching rule dictated by the protocol version.
    pub request_match: RequestMatch,
    /// Identity fields carried by the response. Empty means the exchange was
    /// classified as supported and non-identity-bearing.
    pub fields: Vec<ProtocolIdentityField>,
}

/// Why protocol traffic cannot be proven safe or synthetic.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProtocolIdentityError {
    /// A report has invalid or unsupported framing.
    #[error("malformed or unsupported HID++ report")]
    MalformedReport,
    /// Recorded request and response headers do not correlate under their protocol.
    #[error("request and response do not correlate")]
    CorrelationMismatch,
    /// A HID++ 2.0 ping received a HID++ 1.0 error response.
    #[error("unsupported cross-version HID++ ping")]
    UnsupportedCrossVersionPing,
    /// HID++ 1.0 traffic is outside the proven receiver register layouts.
    #[error("unsupported HID++ 1.0 receiver register")]
    UnsupportedHidpp10Register,
    /// A HID++ 2.0 runtime feature index was never learned.
    #[error("unknown HID++ 2.0 feature index {feature_index:#04x} for device {device_index:#04x}")]
    UnknownFeatureIndex {
        /// Device index carried by the request.
        device_index: u8,
        /// Runtime feature index carried by the request.
        feature_index: u8,
    },
    /// A known identity feature has no safe extractor.
    #[error("unsupported identity-bearing HID++ feature {feature_id:#06x}")]
    UnsupportedIdentityFeature {
        /// HID++ feature ID.
        feature_id: u16,
    },
    /// A feature/function pair is outside the proven read-only classifier.
    #[error("unsupported HID++ function {feature_id:#06x}/{function_id}")]
    UnsupportedHidpp20Function {
        /// HID++ feature ID.
        feature_id: u16,
        /// HID++ function ID.
        function_id: u8,
    },
    /// Feature discovery assigned one runtime index to conflicting feature IDs.
    #[error("ambiguous HID++ runtime feature mapping")]
    AmbiguousFeatureMapping,
    /// An identity field violates its protocol-defined shape.
    #[error("malformed protocol identity field")]
    MalformedIdentity,
    /// Pairing, discovery-address, or passkey traffic is never fixture evidence.
    #[error("pairing or passkey traffic is unsupported")]
    PairingTraffic,
    /// A protocol identity carries real-looking or noncanonical bytes.
    #[error("non-synthetic {kind:?} identity: {source}")]
    NonSyntheticIdentity {
        /// Strong representation expected at the field.
        kind: SyntheticIdentityKind,
        /// Policy rejection.
        source: SyntheticIdentityError,
    },
}

/// Stateful read-only HID++ identity-field extractor.
///
/// Runtime Root and FeatureSet mappings are learned in exchange order. The
/// extractor accepts only the existing recorder's proven read-only traffic;
/// unknown identity-capable features and functions fail closed.
#[derive(Default)]
pub struct ProtocolIdentityExtractor {
    features: BTreeMap<(u8, u8), u16>,
}

impl ProtocolIdentityExtractor {
    /// Inspect an exchange, learn feature mappings, and locate identity fields.
    pub fn inspect_exchange(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<ProtocolExchangeIdentity, ProtocolIdentityError> {
        validate_report(request)?;
        validate_report(response)?;
        let hidpp10 = self.is_hidpp10_request(request);
        if hidpp10 && (is_pairing_report(request) || is_pairing_report(response)) {
            return Err(ProtocolIdentityError::PairingTraffic);
        }

        let fields = if hidpp10 {
            Self::inspect_hidpp10(request, response)?
        } else {
            self.inspect_hidpp20(request, response)?
        };
        Ok(ProtocolExchangeIdentity {
            request_match: if hidpp10 {
                RequestMatch::Exact
            } else {
                RequestMatch::Hidpp20
            },
            fields,
        })
    }

    /// Inspect an exchange and require every nonzero identity field to carry a
    /// canonical synthetic value.
    pub fn classify_exchange(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<Vec<(SyntheticIdentityKind, SyntheticIdentityOrdinal)>, ProtocolIdentityError> {
        let inspection = self.inspect_exchange(request, response)?;
        inspection
            .fields
            .iter()
            .filter_map(|field| match field.value(response) {
                Ok(value) if value.iter().all(|byte| *byte == 0) => None,
                Ok(_) => Some(
                    field
                        .classify(response)
                        .map(|ordinal| (field.kind, ordinal)),
                ),
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    fn inspect_hidpp10(
        request: &[u8],
        response: &[u8],
    ) -> Result<Vec<ProtocolIdentityField>, ProtocolIdentityError> {
        validate_hidpp10_correlation(request, response)?;
        let operation = classify_hidpp10(request)?;
        if is_hidpp10_error(response) {
            require_short(response)?;
            return Ok(Vec::new());
        }

        let field = match operation {
            Hidpp10Operation::ReceiverControl => {
                require_short(response)?;
                None
            }
            Hidpp10Operation::ReceiverUniqueId => {
                require_long(response)?;
                if str::from_utf8(&response[4..20]).is_err() {
                    return Err(ProtocolIdentityError::MalformedIdentity);
                }
                Some(field(SyntheticIdentityKind::BoltReceiverUid, 4..20))
            }
            Hidpp10Operation::ReceiverSerialNumber => {
                validate_receiver_info_response(request, response)?;
                Some(field(SyntheticIdentityKind::UnifyingReceiverSerial, 5..9))
            }
            Hidpp10Operation::DeviceUnitId => {
                validate_receiver_info_response(request, response)?;
                Some(field(SyntheticIdentityKind::DeviceUnitId, 8..12))
            }
            Hidpp10Operation::UnifyingCodename => {
                validate_receiver_info_response(request, response)?;
                validate_utf8_field(response, 6, response[5], 20)?;
                None
            }
            Hidpp10Operation::BoltCodename => {
                validate_receiver_info_response(request, response)?;
                validate_utf8_field(response, 7, response[6], 20)?;
                None
            }
        };
        Ok(field.into_iter().collect())
    }

    fn inspect_hidpp20(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<Vec<ProtocolIdentityField>, ProtocolIdentityError> {
        if is_cross_version_ping(request, response) {
            return Err(ProtocolIdentityError::UnsupportedCrossVersionPing);
        }
        validate_hidpp20_correlation(request, response)?;
        let device_index = request[1];
        let feature_index = request[2];
        let function_id = request[3] >> 4;
        let feature_id = if feature_index == 0 {
            ROOT
        } else {
            *self.features.get(&(device_index, feature_index)).ok_or(
                ProtocolIdentityError::UnknownFeatureIndex {
                    device_index,
                    feature_index,
                },
            )?
        };

        if is_identity_feature(feature_id) {
            return Err(ProtocolIdentityError::UnsupportedIdentityFeature { feature_id });
        }
        if is_hidpp20_error(response) {
            validate_supported_function(feature_id, function_id)?;
            require_short(response)?;
            return Ok(Vec::new());
        }

        let field = match (feature_id, function_id) {
            (ROOT, 0) => {
                self.learn_root_mapping(request, response)?;
                None
            }
            (ROOT, 1) | (FEATURE_SET, 0) | (DEVICE_TYPE_AND_NAME, 0..=2) => None,
            (FEATURE_SET, 1) => {
                self.learn_feature_set_mapping(request, response)?;
                None
            }
            (DEVICE_INFORMATION, 0) => {
                require_long(response)?;
                Some(field(SyntheticIdentityKind::DeviceUnitId, 5..9))
            }
            (DEVICE_INFORMATION, 1) => {
                require_long(response)?;
                None
            }
            (DEVICE_INFORMATION, 2) => {
                require_long(response)?;
                if str::from_utf8(&response[4..16]).is_err() {
                    return Err(ProtocolIdentityError::MalformedIdentity);
                }
                Some(field(SyntheticIdentityKind::DeviceSerialNumber, 4..16))
            }
            _ => {
                validate_supported_function(feature_id, function_id)?;
                None
            }
        };
        Ok(field.into_iter().collect())
    }

    fn learn_root_mapping(
        &mut self,
        request: &[u8],
        response: &[u8],
    ) -> Result<(), ProtocolIdentityError> {
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
    ) -> Result<(), ProtocolIdentityError> {
        let feature_index = request[4];
        let feature_id = u16::from_be_bytes([response[4], response[5]]);
        if feature_index == 0 {
            return if feature_id == ROOT {
                Ok(())
            } else {
                Err(ProtocolIdentityError::MalformedReport)
            };
        }
        self.insert_feature(request[1], feature_index, feature_id)
    }

    fn insert_feature(
        &mut self,
        device_index: u8,
        feature_index: u8,
        feature_id: u16,
    ) -> Result<(), ProtocolIdentityError> {
        match self
            .features
            .insert((device_index, feature_index), feature_id)
        {
            Some(previous) if previous != feature_id => {
                Err(ProtocolIdentityError::AmbiguousFeatureMapping)
            }
            _ => Ok(()),
        }
    }

    fn is_hidpp10_request(&self, report: &[u8]) -> bool {
        report[1] == 0xff
            && matches!(report[2], 0x80..=0x83)
            && !self.features.contains_key(&(report[1], report[2]))
    }
}

/// Whether a report belongs to pairing, discovery-address, or passkey traffic.
#[must_use]
pub fn is_pairing_identity_traffic(report: &[u8]) -> bool {
    is_pairing_report(report)
}

fn field(kind: SyntheticIdentityKind, range: Range<usize>) -> ProtocolIdentityField {
    ProtocolIdentityField { kind, range }
}

fn validate_report(report: &[u8]) -> Result<(), ProtocolIdentityError> {
    matches!(
        (report.first(), report.len()),
        (Some(0x10), 7) | (Some(0x11), 20)
    )
    .then_some(())
    .ok_or(ProtocolIdentityError::MalformedReport)
}

fn classify_hidpp10(request: &[u8]) -> Result<Hidpp10Operation, ProtocolIdentityError> {
    require_short(request)?;
    match (request[2], request[3], request[4]) {
        (0x80 | 0x81, 0x00 | 0x02, _) => Ok(Hidpp10Operation::ReceiverControl),
        (0x83, 0xfb, _) => Ok(Hidpp10Operation::ReceiverUniqueId),
        (0x83, 0xb5, 0x03) => Ok(Hidpp10Operation::ReceiverSerialNumber),
        (0x83, 0xb5, 0x51..=0x56) => Ok(Hidpp10Operation::DeviceUnitId),
        (0x83, 0xb5, 0x40..=0x45) => Ok(Hidpp10Operation::UnifyingCodename),
        (0x83, 0xb5, 0x61..=0x66) => Ok(Hidpp10Operation::BoltCodename),
        _ => Err(ProtocolIdentityError::UnsupportedHidpp10Register),
    }
}

fn validate_receiver_info_response(
    request: &[u8],
    response: &[u8],
) -> Result<(), ProtocolIdentityError> {
    require_long(response)?;
    (response[4] == request[4])
        .then_some(())
        .ok_or(ProtocolIdentityError::CorrelationMismatch)
}

fn validate_hidpp10_correlation(
    request: &[u8],
    response: &[u8],
) -> Result<(), ProtocolIdentityError> {
    let normal =
        response[1] == request[1] && response[2] == request[2] && response[3] == request[3];
    let error = response[1] == request[1]
        && response[2] == 0x8f
        && response[3] == request[2]
        && response[4] == request[3];
    (normal || error)
        .then_some(())
        .ok_or(ProtocolIdentityError::CorrelationMismatch)
}

fn validate_hidpp20_correlation(
    request: &[u8],
    response: &[u8],
) -> Result<(), ProtocolIdentityError> {
    let normal =
        response[1] == request[1] && response[2] == request[2] && response[3] == request[3];
    let error = response[1] == request[1]
        && response[2] == 0xff
        && response[3] == request[2]
        && response[4] == request[3];
    (normal || error)
        .then_some(())
        .ok_or(ProtocolIdentityError::CorrelationMismatch)
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

fn require_long(report: &[u8]) -> Result<(), ProtocolIdentityError> {
    (report[0] == 0x11)
        .then_some(())
        .ok_or(ProtocolIdentityError::MalformedIdentity)
}

fn require_short(report: &[u8]) -> Result<(), ProtocolIdentityError> {
    (report[0] == 0x10)
        .then_some(())
        .ok_or(ProtocolIdentityError::MalformedReport)
}

fn validate_utf8_field(
    response: &[u8],
    start: usize,
    length: u8,
    limit: usize,
) -> Result<(), ProtocolIdentityError> {
    let end = start
        .checked_add(usize::from(length))
        .filter(|end| *end <= limit)
        .ok_or(ProtocolIdentityError::MalformedReport)?;
    str::from_utf8(&response[start..end])
        .map(|_| ())
        .map_err(|_| ProtocolIdentityError::MalformedReport)
}

fn validate_supported_function(
    feature_id: u16,
    function_id: u8,
) -> Result<(), ProtocolIdentityError> {
    let supported = match feature_id {
        ROOT | FEATURE_SET | 0x1004 | 0x2111 | 0x2150 => matches!(function_id, 0..=1),
        DEVICE_INFORMATION | DEVICE_TYPE_AND_NAME | 0x2201 => matches!(function_id, 0..=2),
        0x1000 | 0x1001 | 0x2100 | 0x2110 => function_id == 0,
        0x1982 => matches!(function_id, 0 | 2),
        0x1b04 => matches!(function_id, 0..=2 | 4),
        0x2121 => matches!(function_id, 0..=1 | 3),
        0x2202 => matches!(function_id, 0..=5 | 8),
        0x6501 => matches!(function_id, 0 | 3),
        _ => false,
    };
    supported
        .then_some(())
        .ok_or(ProtocolIdentityError::UnsupportedHidpp20Function {
            feature_id,
            function_id,
        })
}
