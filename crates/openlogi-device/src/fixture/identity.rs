use std::num::NonZeroU8;

use thiserror::Error;

const BOLT_PREFIX: &[u8; 12] = b"OL-BOLT-UID-";
const DEVICE_SERIAL_PREFIX: &[u8; 7] = b"OL-SER-";
const RAW_HID_PREFIX: &str = "OPENLOGI-FIXTURE-RAWHID-";
const UNIFYING_MAGIC: [u8; 3] = *b"OLR";
const DEVICE_UNIT_MAGIC: [u8; 3] = *b"OLD";

/// Maximum ordinal supported by every synthetic identity representation.
pub const MAX_SYNTHETIC_IDENTITY_ORDINAL: u16 = u8::MAX as u16;

/// A protocol- or profile-specific synthetic identity representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyntheticIdentityKind {
    /// Bolt receiver UID from HID++ 1.0 register `0xfb`.
    BoltReceiverUid,
    /// Four binary serial bytes from Unifying receiver register `0xb5/0x03`.
    UnifyingReceiverSerial,
    /// Uppercase hexadecimal route form of a Unifying receiver serial.
    UnifyingReceiverRoute,
    /// Four-byte HID++ device unit ID.
    DeviceUnitId,
    /// Twelve-byte HID++ DeviceInformation serial.
    DeviceSerialNumber,
    /// Opaque raw-HID identity retained in a semantic profile and route.
    RawHidProfileIdentity,
}

/// A bounded, nonzero synthetic identity ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyntheticIdentityOrdinal(NonZeroU8);

impl SyntheticIdentityOrdinal {
    /// Construct an ordinal accepted by every supported representation.
    pub fn new(value: u16) -> Result<Self, SyntheticIdentityError> {
        let value = u8::try_from(value)
            .ok()
            .and_then(NonZeroU8::new)
            .ok_or(SyntheticIdentityError::InvalidOrdinal { value })?;
        Ok(Self(value))
    }

    /// Return the numeric ordinal.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }
}

/// One generated or classified strongly tagged synthetic value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyntheticIdentityValue {
    /// Fixed-width, tagged Bolt receiver UID.
    BoltReceiverUid([u8; 16]),
    /// Tagged four-byte Unifying receiver serial.
    UnifyingReceiverSerial([u8; 4]),
    /// Uppercase hexadecimal route derived from a tagged Unifying serial.
    UnifyingReceiverRoute(String),
    /// Tagged four-byte device unit ID.
    DeviceUnitId([u8; 4]),
    /// Fixed-width, tagged DeviceInformation serial.
    DeviceSerialNumber([u8; 12]),
    /// Explicitly tagged raw-HID profile identity.
    RawHidProfileIdentity(String),
}

impl SyntheticIdentityValue {
    /// Return this value's representation tag.
    #[must_use]
    pub const fn kind(&self) -> SyntheticIdentityKind {
        match self {
            Self::BoltReceiverUid(_) => SyntheticIdentityKind::BoltReceiverUid,
            Self::UnifyingReceiverSerial(_) => SyntheticIdentityKind::UnifyingReceiverSerial,
            Self::UnifyingReceiverRoute(_) => SyntheticIdentityKind::UnifyingReceiverRoute,
            Self::DeviceUnitId(_) => SyntheticIdentityKind::DeviceUnitId,
            Self::DeviceSerialNumber(_) => SyntheticIdentityKind::DeviceSerialNumber,
            Self::RawHidProfileIdentity(_) => SyntheticIdentityKind::RawHidProfileIdentity,
        }
    }

    /// Return the protocol bytes for a byte-backed representation.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::BoltReceiverUid(value) => Some(value),
            Self::UnifyingReceiverSerial(value) | Self::DeviceUnitId(value) => Some(value),
            Self::DeviceSerialNumber(value) => Some(value),
            Self::UnifyingReceiverRoute(_) | Self::RawHidProfileIdentity(_) => None,
        }
    }

    /// Return the profile string for a string-backed representation.
    #[must_use]
    pub fn as_profile_str(&self) -> Option<&str> {
        match self {
            Self::UnifyingReceiverRoute(value) | Self::RawHidProfileIdentity(value) => Some(value),
            Self::BoltReceiverUid(value) => str::from_utf8(value).ok(),
            Self::DeviceSerialNumber(value) => str::from_utf8(value).ok(),
            Self::UnifyingReceiverSerial(_) | Self::DeviceUnitId(_) => None,
        }
    }
}

/// A synthetic identity generation or classification failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SyntheticIdentityError {
    /// Ordinals are bounded to the nonzero range shared by all representations.
    #[error("synthetic identity ordinal {value} is outside 1..={MAX_SYNTHETIC_IDENTITY_ORDINAL}")]
    InvalidOrdinal {
        /// Rejected ordinal.
        value: u16,
    },
    /// A value does not carry the exact canonical tag and formatting for its kind.
    #[error("value is not a canonical synthetic {kind:?}")]
    InvalidValue {
        /// Representation that was expected.
        kind: SyntheticIdentityKind,
    },
    /// The byte classifier was asked to handle a profile-string-only representation.
    #[error("{kind:?} is not a byte-backed synthetic identity")]
    NotByteBacked {
        /// Representation rejected by the byte classifier.
        kind: SyntheticIdentityKind,
    },
    /// The profile classifier was asked to handle a binary-only representation.
    #[error("{kind:?} is not a profile-string synthetic identity")]
    NotProfileString {
        /// Representation rejected by the profile classifier.
        kind: SyntheticIdentityKind,
    },
}

/// Generate one deterministic synthetic identity from a kind and bounded ordinal.
#[must_use]
pub fn generate_synthetic_identity(
    kind: SyntheticIdentityKind,
    ordinal: SyntheticIdentityOrdinal,
) -> SyntheticIdentityValue {
    let ordinal = ordinal.get();
    match kind {
        SyntheticIdentityKind::BoltReceiverUid => {
            let mut value = [0; 16];
            value[..BOLT_PREFIX.len()].copy_from_slice(BOLT_PREFIX);
            write_decimal(&mut value[BOLT_PREFIX.len()..], ordinal);
            SyntheticIdentityValue::BoltReceiverUid(value)
        }
        SyntheticIdentityKind::UnifyingReceiverSerial => {
            SyntheticIdentityValue::UnifyingReceiverSerial([
                UNIFYING_MAGIC[0],
                UNIFYING_MAGIC[1],
                UNIFYING_MAGIC[2],
                ordinal,
            ])
        }
        SyntheticIdentityKind::UnifyingReceiverRoute => {
            let serial = [
                UNIFYING_MAGIC[0],
                UNIFYING_MAGIC[1],
                UNIFYING_MAGIC[2],
                ordinal,
            ];
            SyntheticIdentityValue::UnifyingReceiverRoute(unifying_receiver_route(serial))
        }
        SyntheticIdentityKind::DeviceUnitId => SyntheticIdentityValue::DeviceUnitId([
            DEVICE_UNIT_MAGIC[0],
            DEVICE_UNIT_MAGIC[1],
            DEVICE_UNIT_MAGIC[2],
            ordinal,
        ]),
        SyntheticIdentityKind::DeviceSerialNumber => {
            let mut value = [0; 12];
            value[..DEVICE_SERIAL_PREFIX.len()].copy_from_slice(DEVICE_SERIAL_PREFIX);
            write_decimal(&mut value[DEVICE_SERIAL_PREFIX.len()..], ordinal);
            SyntheticIdentityValue::DeviceSerialNumber(value)
        }
        SyntheticIdentityKind::RawHidProfileIdentity => {
            SyntheticIdentityValue::RawHidProfileIdentity(format!("{RAW_HID_PREFIX}{ordinal:03}"))
        }
    }
}

/// Classify canonical synthetic bytes and recover their ordinal.
pub fn classify_synthetic_identity_bytes(
    kind: SyntheticIdentityKind,
    value: &[u8],
) -> Result<SyntheticIdentityOrdinal, SyntheticIdentityError> {
    let ordinal = match kind {
        SyntheticIdentityKind::BoltReceiverUid => classify_tagged_decimal(value, BOLT_PREFIX, 4),
        SyntheticIdentityKind::UnifyingReceiverSerial => classify_magic(value, UNIFYING_MAGIC),
        SyntheticIdentityKind::DeviceUnitId => classify_magic(value, DEVICE_UNIT_MAGIC),
        SyntheticIdentityKind::DeviceSerialNumber => {
            classify_tagged_decimal(value, DEVICE_SERIAL_PREFIX, 5)
        }
        SyntheticIdentityKind::UnifyingReceiverRoute
        | SyntheticIdentityKind::RawHidProfileIdentity => {
            return Err(SyntheticIdentityError::NotByteBacked { kind });
        }
    }
    .ok_or(SyntheticIdentityError::InvalidValue { kind })?;
    SyntheticIdentityOrdinal::new(u16::from(ordinal))
        .map_err(|_| SyntheticIdentityError::InvalidValue { kind })
}

/// Classify a canonical synthetic profile string and recover its ordinal.
pub fn classify_synthetic_profile_identity(
    kind: SyntheticIdentityKind,
    value: &str,
) -> Result<SyntheticIdentityOrdinal, SyntheticIdentityError> {
    match kind {
        SyntheticIdentityKind::BoltReceiverUid | SyntheticIdentityKind::DeviceSerialNumber => {
            classify_synthetic_identity_bytes(kind, value.as_bytes())
        }
        SyntheticIdentityKind::UnifyingReceiverRoute => {
            let serial = parse_upper_hex_4(value)
                .filter(|serial| serial[..3] == UNIFYING_MAGIC)
                .ok_or(SyntheticIdentityError::InvalidValue { kind })?;
            SyntheticIdentityOrdinal::new(u16::from(serial[3]))
                .map_err(|_| SyntheticIdentityError::InvalidValue { kind })
        }
        SyntheticIdentityKind::RawHidProfileIdentity => {
            let suffix = value
                .strip_prefix(RAW_HID_PREFIX)
                .filter(|suffix| suffix.len() == 3)
                .and_then(parse_decimal)
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(SyntheticIdentityError::InvalidValue { kind })?;
            SyntheticIdentityOrdinal::new(u16::from(suffix))
                .map_err(|_| SyntheticIdentityError::InvalidValue { kind })
        }
        SyntheticIdentityKind::UnifyingReceiverSerial | SyntheticIdentityKind::DeviceUnitId => {
            Err(SyntheticIdentityError::NotProfileString { kind })
        }
    }
}

/// Format a Unifying binary serial as the uppercase hexadecimal profile route.
#[must_use]
pub fn unifying_receiver_route(serial: [u8; 4]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}",
        serial[0], serial[1], serial[2], serial[3]
    )
}

fn write_decimal(target: &mut [u8], value: u8) {
    let mut value = u16::from(value);
    for digit in target.iter_mut().rev() {
        *digit = b'0' + (value % 10).to_le_bytes()[0];
        value /= 10;
    }
}

fn classify_tagged_decimal(value: &[u8], prefix: &[u8], digits: usize) -> Option<u8> {
    let suffix = value.strip_prefix(prefix)?;
    if suffix.len() != digits {
        return None;
    }
    parse_decimal(str::from_utf8(suffix).ok()?).and_then(|value| u8::try_from(value).ok())
}

fn classify_magic(value: &[u8], magic: [u8; 3]) -> Option<u8> {
    let value: [u8; 4] = value.try_into().ok()?;
    (value[..3] == magic).then_some(value[3])
}

fn parse_decimal(value: &str) -> Option<u16> {
    value.bytes().try_fold(0u16, |number, digit| {
        digit
            .is_ascii_digit()
            .then_some(u16::from(digit - b'0'))
            .and_then(|digit| number.checked_mul(10)?.checked_add(digit))
    })
}

fn parse_upper_hex_4(value: &str) -> Option<[u8; 4]> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    if pairs.len() != 4 || !remainder.is_empty() {
        return None;
    }
    let bytes: Vec<_> = pairs
        .iter()
        .map(|pair| Some(upper_hex_digit(pair[0])? << 4 | upper_hex_digit(pair[1])?))
        .collect::<Option<_>>()?;
    bytes.try_into().ok()
}

fn upper_hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}
