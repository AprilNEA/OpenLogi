//! Validated, canonical identifiers used for cross-transport device correlation.

use crate::proto::openlogi::flow::v1::{
    DeviceIdentifier as WireDeviceIdentifier, DeviceIdentity, IdentifierKind,
};
use thiserror::Error;

/// A validated device identifier in its canonical wire representation.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CanonicalDeviceIdentifier {
    /// A serial number represented by its exact UTF-8 bytes.
    Serial(String),
    /// A HID++ unit ID represented canonically as a 32-bit value.
    UnitId(u32),
    /// A Bluetooth address in transmission order (most-significant byte first).
    BluetoothAddress([u8; 6]),
}

impl CanonicalDeviceIdentifier {
    /// Creates a canonical serial identifier without padding or case folding.
    #[must_use]
    pub fn serial(serial: impl Into<String>) -> Self {
        Self::Serial(serial.into())
    }

    /// Creates a canonical HID++ unit-ID identifier.
    #[must_use]
    pub const fn unit_id(unit_id: u32) -> Self {
        Self::UnitId(unit_id)
    }

    /// Creates a canonical Bluetooth-address identifier from MSB-first bytes.
    #[must_use]
    pub const fn bluetooth_address(address: [u8; 6]) -> Self {
        Self::BluetoothAddress(address)
    }

    /// Returns the generated protocol identifier kind.
    #[must_use]
    pub const fn kind(&self) -> IdentifierKind {
        match self {
            Self::Serial(_) => IdentifierKind::Serial,
            Self::UnitId(_) => IdentifierKind::UnitId,
            Self::BluetoothAddress(_) => IdentifierKind::BluetoothAddress,
        }
    }

    /// Returns this identifier's canonical wire bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            Self::Serial(serial) => serial.as_bytes().to_vec(),
            Self::UnitId(unit_id) => unit_id.to_be_bytes().to_vec(),
            Self::BluetoothAddress(address) => address.to_vec(),
        }
    }
}

impl TryFrom<&WireDeviceIdentifier> for CanonicalDeviceIdentifier {
    type Error = DeviceIdentifierError;

    fn try_from(identifier: &WireDeviceIdentifier) -> Result<Self, Self::Error> {
        match identifier.kind.as_known() {
            Some(IdentifierKind::Serial) => {
                let serial = std::str::from_utf8(&identifier.value)
                    .map_err(|_| DeviceIdentifierError::InvalidSerialUtf8)?;
                if serial.is_empty() {
                    return Err(DeviceIdentifierError::EmptySerial);
                }
                Ok(Self::Serial(serial.to_owned()))
            }
            Some(IdentifierKind::UnitId) => {
                let bytes = exact_bytes::<4>(IdentifierKind::UnitId, &identifier.value)?;
                Ok(Self::UnitId(u32::from_be_bytes(bytes)))
            }
            Some(IdentifierKind::BluetoothAddress) => {
                let bytes = exact_bytes::<6>(IdentifierKind::BluetoothAddress, &identifier.value)?;
                Ok(Self::BluetoothAddress(bytes))
            }
            Some(IdentifierKind::Unspecified) => Err(DeviceIdentifierError::UnspecifiedKind),
            None => Err(DeviceIdentifierError::UnknownKind(identifier.kind.to_i32())),
        }
    }
}

impl From<CanonicalDeviceIdentifier> for WireDeviceIdentifier {
    fn from(identifier: CanonicalDeviceIdentifier) -> Self {
        let (kind, value) = match identifier {
            CanonicalDeviceIdentifier::Serial(serial) => {
                (IdentifierKind::Serial, serial.into_bytes())
            }
            CanonicalDeviceIdentifier::UnitId(unit_id) => {
                (IdentifierKind::UnitId, unit_id.to_be_bytes().to_vec())
            }
            CanonicalDeviceIdentifier::BluetoothAddress(address) => {
                (IdentifierKind::BluetoothAddress, address.to_vec())
            }
        };
        Self {
            kind: kind.into(),
            value,
            ..Self::default()
        }
    }
}

/// A validation error for a generated device identifier.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DeviceIdentifierError {
    /// The required identifier kind was omitted.
    #[error("identifier kind is unspecified")]
    UnspecifiedKind,
    /// The identifier kind is newer than this implementation understands.
    #[error("unknown identifier kind {0}")]
    UnknownKind(i32),
    /// A serial identifier does not contain valid UTF-8.
    #[error("serial identifier is not valid UTF-8")]
    InvalidSerialUtf8,
    /// A serial identifier contains no bytes and cannot identify a device.
    #[error("serial identifier is empty")]
    EmptySerial,
    /// A fixed-width identifier has a noncanonical byte count.
    #[error("{kind:?} identifier must be {expected} bytes, received {actual}")]
    InvalidLength {
        /// The kind whose value was malformed.
        kind: IdentifierKind,
        /// The canonical byte count.
        expected: usize,
        /// The supplied byte count.
        actual: usize,
    },
}

/// Returns whether two generated identities denote the same physical device.
///
/// Every identifier in both identities is validated first. Malformed,
/// unspecified, or unknown identifiers prevent correlation and return an error.
pub fn same_device(
    first: &DeviceIdentity,
    second: &DeviceIdentity,
) -> Result<bool, DeviceIdentifierError> {
    let first = validated_identifiers(first)?;
    let second = validated_identifiers(second)?;
    Ok(first.iter().any(|identifier| second.contains(identifier)))
}

fn validated_identifiers(
    identity: &DeviceIdentity,
) -> Result<Vec<CanonicalDeviceIdentifier>, DeviceIdentifierError> {
    identity.ids.iter().map(TryFrom::try_from).collect()
}

fn exact_bytes<const N: usize>(
    kind: IdentifierKind,
    value: &[u8],
) -> Result<[u8; N], DeviceIdentifierError> {
    value
        .try_into()
        .map_err(|_| DeviceIdentifierError::InvalidLength {
            kind,
            expected: N,
            actual: value.len(),
        })
}

#[cfg(test)]
mod tests {
    use super::{CanonicalDeviceIdentifier, DeviceIdentifierError, same_device};
    use crate::proto::openlogi::flow::v1::{DeviceIdentifier, DeviceIdentity, IdentifierKind};

    fn identity(ids: Vec<CanonicalDeviceIdentifier>) -> DeviceIdentity {
        DeviceIdentity {
            ids: ids.into_iter().map(Into::into).collect(),
            ..DeviceIdentity::default()
        }
    }

    #[test]
    fn canonical_constructors_encode_protocol_bytes() {
        let serial: DeviceIdentifier = CanonicalDeviceIdentifier::serial("Mx-123").into();
        let unit: DeviceIdentifier = CanonicalDeviceIdentifier::unit_id(0x1234_abcd).into();
        let bluetooth: DeviceIdentifier =
            CanonicalDeviceIdentifier::bluetooth_address([1, 2, 3, 4, 5, 6]).into();

        assert_eq!(serial.value, b"Mx-123");
        assert_eq!(unit.value, [0x12, 0x34, 0xab, 0xcd]);
        assert_eq!(bluetooth.value, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn nonintersecting_identifiers_do_not_match() {
        let first = identity(vec![CanonicalDeviceIdentifier::unit_id(1)]);
        let second = identity(vec![CanonicalDeviceIdentifier::unit_id(2)]);
        assert!(!same_device(&first, &second).unwrap());
    }

    #[test]
    fn any_identifier_intersection_matches() {
        let first = identity(vec![
            CanonicalDeviceIdentifier::serial("first"),
            CanonicalDeviceIdentifier::unit_id(42),
        ]);
        let second = identity(vec![
            CanonicalDeviceIdentifier::serial("second"),
            CanonicalDeviceIdentifier::unit_id(42),
        ]);
        assert!(same_device(&first, &second).unwrap());
    }

    #[test]
    fn malformed_identifier_is_rejected() {
        let malformed = DeviceIdentifier {
            kind: IdentifierKind::UnitId.into(),
            value: vec![0, 1, 2],
            ..DeviceIdentifier::default()
        };
        let first = DeviceIdentity {
            ids: vec![malformed],
            ..DeviceIdentity::default()
        };

        assert_eq!(
            same_device(&first, &DeviceIdentity::default()).unwrap_err(),
            DeviceIdentifierError::InvalidLength {
                kind: IdentifierKind::UnitId,
                expected: 4,
                actual: 3,
            }
        );
    }

    #[test]
    fn empty_serial_is_rejected() {
        let empty = DeviceIdentifier {
            kind: IdentifierKind::Serial.into(),
            ..DeviceIdentifier::default()
        };

        assert_eq!(
            CanonicalDeviceIdentifier::try_from(&empty).unwrap_err(),
            DeviceIdentifierError::EmptySerial
        );
    }

    #[test]
    fn unspecified_and_unknown_kinds_are_rejected() {
        let unspecified = DeviceIdentifier::default();
        let unknown = DeviceIdentifier {
            kind: 99.into(),
            ..DeviceIdentifier::default()
        };

        assert_eq!(
            CanonicalDeviceIdentifier::try_from(&unspecified).unwrap_err(),
            DeviceIdentifierError::UnspecifiedKind
        );
        assert_eq!(
            CanonicalDeviceIdentifier::try_from(&unknown).unwrap_err(),
            DeviceIdentifierError::UnknownKind(99)
        );
    }
}
