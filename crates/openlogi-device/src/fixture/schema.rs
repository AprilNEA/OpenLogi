use std::collections::HashSet;

use openlogi_core::device::DeviceInventory;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version supported by the initial profile and cassette formats.
pub const FIXTURE_SCHEMA_VERSION: u32 = 1;

/// A fixture schema, validation, or replay failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FixtureError {
    /// The asset uses a schema version this build does not understand.
    #[error("unsupported {asset} schema version {actual}; expected {supported}")]
    UnsupportedSchema {
        /// Human-readable asset kind.
        asset: &'static str,
        /// Version found in the asset.
        actual: u32,
        /// Version supported by this build.
        supported: u32,
    },
    /// The asset violates a schema invariant.
    #[error("invalid {asset}: {message}")]
    InvalidAsset {
        /// Human-readable asset kind.
        asset: &'static str,
        /// Specific failed invariant.
        message: String,
    },
    /// No pending cassette exchange matched an outgoing report.
    #[error("unmatched HID request: actual={actual}, hidpp20_normalized={normalized}")]
    UnmatchedRequest {
        /// Exact outgoing bytes as lowercase hex.
        actual: String,
        /// The same bytes with only the HID++ 2.0 software-ID nibble cleared.
        normalized: String,
    },
    /// One or more required cassette exchanges were not consumed.
    #[error("required cassette exchanges were not consumed: {requests:?}")]
    UnconsumedExchanges {
        /// Normalized request keys that remained pending.
        requests: Vec<String>,
    },
    /// A topology operation named a node that does not exist.
    #[error("unknown replay node {0}")]
    UnknownNode(String),
    /// A topology operation named a logical channel that does not exist.
    #[error("unknown replay channel {0}")]
    UnknownChannel(String),
}

impl FixtureError {
    pub(super) fn invalid(asset: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidAsset {
            asset,
            message: message.into(),
        }
    }
}

/// A semantic, host-independent snapshot of one synthetic specimen.
///
/// Raw reports and mutable topology do not belong here. The profile is the
/// independently reviewable expectation consumed by semantic mocks and tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    /// Profile schema version.
    pub schema_version: u32,
    /// Stable synthetic specimen identifier, never a hardware serial.
    pub id: String,
    /// Human-readable specimen name.
    pub name: String,
    /// Expected semantic device inventories.
    pub inventories: Vec<DeviceInventory>,
}

impl DeviceProfile {
    /// Validate the schema version and minimal semantic invariants.
    pub fn validate(&self) -> Result<(), FixtureError> {
        validate_version("device profile", self.schema_version)?;
        validate_name("device profile", "id", &self.id)?;
        validate_name("device profile", "name", &self.name)?;
        if self.inventories.is_empty() {
            return Err(FixtureError::invalid(
                "device profile",
                "inventories must not be empty",
            ));
        }
        for inventory in &self.inventories {
            let mut slots = HashSet::new();
            for device in &inventory.paired {
                if !slots.insert(device.slot) {
                    return Err(FixtureError::invalid(
                        "device profile",
                        format!(
                            "receiver {:04x}:{:04x} repeats slot {}",
                            inventory.receiver.vendor_id,
                            inventory.receiver.product_id,
                            device.slot
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// HID report widths exposed by one logical replay channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportSupport {
    /// Both seven-byte (`0x10`) and twenty-byte (`0x11`) HID++ reports.
    ShortAndLong,
    /// Only twenty-byte (`0x11`) HID++ reports; short requests are widened by
    /// the production channel before reaching replay.
    LongOnly,
}

impl ReportSupport {
    pub(super) const fn flags(self) -> (bool, bool) {
        match self {
            Self::ShortAndLong => (true, true),
            Self::LongOnly => (false, true),
        }
    }
}

/// How one outgoing cassette request is keyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMatch {
    /// Match every request byte exactly. HID++ 1.0 exchanges use this mode so
    /// register address and correlation fields remain protocol-specific.
    Exact,
    /// Match a HID++ 2.0 report after clearing only byte 3's software-ID low
    /// nibble. No arbitrary masks are part of the schema.
    Hidpp20,
}

/// One required or optional request/response exchange in a raw HID cassette.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CassetteExchange {
    /// Matching rule for the outgoing request.
    pub request_match: RequestMatch,
    /// Exact outgoing report, including report ID; serde uses lowercase hex.
    #[serde(with = "hex_report")]
    pub request: Vec<u8>,
    /// Incoming report as lowercase hex, or `None` for a matched write.
    #[serde(default, with = "optional_hex_report")]
    pub response: Option<Vec<u8>>,
    /// Whether completion fails if this exchange remains unused.
    pub required: bool,
}

/// A named raw-HID operation captured on one logical channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HidCassette {
    /// Cassette schema version.
    pub schema_version: u32,
    /// Human-readable operation name.
    pub name: String,
    /// Logical channel identifier referenced by replay topology.
    pub channel: String,
    /// HID++ report widths exposed by the recorded channel.
    pub report_support: ReportSupport,
    /// Request-keyed exchanges. Repeated keys are consumed FIFO.
    pub exchanges: Vec<CassetteExchange>,
}

impl HidCassette {
    /// Validate schema version, report framing, and HID++ 2.0 correlation.
    pub fn validate(&self) -> Result<(), FixtureError> {
        validate_version("HID cassette", self.schema_version)?;
        validate_name("HID cassette", "name", &self.name)?;
        validate_name("HID cassette", "channel", &self.channel)?;
        if self.exchanges.is_empty() {
            return Err(FixtureError::invalid(
                "HID cassette",
                "exchanges must not be empty",
            ));
        }
        for (index, exchange) in self.exchanges.iter().enumerate() {
            validate_report(&exchange.request, self.report_support).map_err(|message| {
                FixtureError::invalid(
                    "HID cassette",
                    format!("exchange {index} request {message}"),
                )
            })?;
            if let Some(response) = &exchange.response {
                validate_report(response, self.report_support).map_err(|message| {
                    FixtureError::invalid(
                        "HID cassette",
                        format!("exchange {index} response {message}"),
                    )
                })?;
            }
            if exchange.request_match == RequestMatch::Hidpp20 {
                validate_hidpp20(exchange, index)?;
            }
        }
        Ok(())
    }
}

fn validate_version(asset: &'static str, actual: u32) -> Result<(), FixtureError> {
    if actual == FIXTURE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(FixtureError::UnsupportedSchema {
            asset,
            actual,
            supported: FIXTURE_SCHEMA_VERSION,
        })
    }
}

fn validate_name(asset: &'static str, field: &str, value: &str) -> Result<(), FixtureError> {
    if value.trim().is_empty() {
        Err(FixtureError::invalid(
            asset,
            format!("{field} must not be empty"),
        ))
    } else {
        Ok(())
    }
}

fn validate_report(report: &[u8], support: ReportSupport) -> Result<(), String> {
    let expected = match report.first() {
        Some(0x10) => 7,
        Some(0x11) => 20,
        Some(0x12) => 64,
        Some(id) => return Err(format!("uses unsupported report id 0x{id:02x}")),
        None => return Err("is empty".to_string()),
    };
    if report.len() != expected {
        return Err(format!(
            "has length {}, expected {expected} for report id 0x{:02x}",
            report.len(),
            report[0]
        ));
    }
    if support == ReportSupport::LongOnly && report[0] == 0x10 {
        return Err("uses a short report on a long-only channel".to_string());
    }
    Ok(())
}

fn validate_hidpp20(exchange: &CassetteExchange, index: usize) -> Result<(), FixtureError> {
    let request = &exchange.request;
    if !matches!(request[0], 0x10 | 0x11) {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} applies HID++ 2.0 matching to a non-HID++ report"),
        ));
    }
    let Some(response) = exchange.response.as_deref() else {
        return Ok(());
    };
    if !matches!(response[0], 0x10 | 0x11) {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} has a non-HID++ 2.0 response"),
        ));
    }
    if response[1] != request[1] {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} response changes the device index"),
        ));
    }
    let correlated = if response[2] == 0xff {
        response[3] == request[2] && response[4] & 0xf0 == request[3] & 0xf0
    } else {
        response[2] == request[2] && response[3] & 0xf0 == request[3] & 0xf0
    };
    if !correlated {
        return Err(FixtureError::invalid(
            "HID cassette",
            format!("exchange {index} response is not correlated to its request"),
        ));
    }
    Ok(())
}

pub(super) fn normalize_hidpp20(request: &[u8]) -> Vec<u8> {
    let mut normalized = request.to_vec();
    if normalized.len() >= 4 && matches!(normalized[0], 0x10 | 0x11) {
        normalized[3] &= 0xf0;
    }
    normalized
}

pub(super) fn format_hex(report: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut formatted = String::with_capacity(report.len() * 2);
    for &byte in report {
        formatted.push(char::from(HEX[usize::from(byte >> 4)]));
        formatted.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    formatted
}

fn parse_hex(encoded: &str) -> Result<Vec<u8>, &'static str> {
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("hex report must contain an even number of digits");
    }
    pairs
        .iter()
        .map(|pair| {
            let high = hex_digit(pair[0])?;
            let low = hex_digit(pair[1])?;
            Ok(high << 4 | low)
        })
        .collect()
}

fn hex_digit(digit: u8) -> Result<u8, &'static str> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        _ => Err("hex report must use lowercase hexadecimal digits"),
    }
}

mod hex_report {
    use serde::{Deserialize, Deserializer, Serializer, de};

    use super::{format_hex, parse_hex};

    pub fn serialize<S>(report: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_hex(report))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        parse_hex(&encoded).map_err(de::Error::custom)
    }
}

mod optional_hex_report {
    use serde::{Deserialize, Deserializer, Serializer, de};

    use super::{format_hex, parse_hex};

    #[expect(
        clippy::ref_option,
        reason = "serde field serializers must receive the field by reference"
    )]
    pub fn serialize<S>(report: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match report {
            Some(report) => serializer.serialize_some(&format_hex(report)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|encoded| parse_hex(&encoded).map_err(de::Error::custom))
            .transpose()
    }
}
