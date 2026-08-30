use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    FIXTURE_SCHEMA_VERSION, FixtureError, SyntheticIdentityKind, classify_synthetic_identity_bytes,
    classify_synthetic_profile_identity, unifying_receiver_route,
};

/// Fixture-scoped manifest and identity ledger.
///
/// This schema contains no provenance, host paths, timestamps, original
/// identities, or hashes. Exact typed occurrences are the evidence contract;
/// there is deliberately no `sanitized` boolean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureManifest {
    /// Fixture schema version shared with the unreleased v1 profile/cassette schemas.
    pub schema_version: u32,
    /// Stable synthetic specimen ID, never a hardware identity.
    pub id: String,
    /// [`super::DeviceProfile::id`] this manifest describes.
    pub profile_id: String,
    /// Named cassette relationships. An empty list truthfully describes a
    /// profile-only synthetic fixture.
    pub cases: Vec<FixtureCase>,
    /// Logical principals and every identity representation attributed to them.
    pub identity_ledger: Vec<IdentityLedgerEntry>,
}

/// One named cassette's profile relationship and logical channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureCase {
    /// Cassette name.
    pub name: String,
    /// Logical replay channel named by the cassette.
    pub channel: String,
    /// Receiver or device whose operation this case represents.
    pub relationship: FixtureCaseRelationship,
}

/// Logical principal anchoring one cassette to the semantic profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureCaseRelationship {
    /// Receiver-level operation; devices routed through this receiver are related too.
    Receiver {
        /// Fixture-local receiver principal ID.
        receiver: String,
    },
    /// Device-level operation; its receiver principal is related when present.
    Device {
        /// Fixture-local device principal ID.
        device: String,
    },
}

/// One logical receiver or device and all of its typed synthetic representations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityLedgerEntry {
    /// Logical principal these values identify.
    pub principal: FixturePrincipal,
    /// Strongly tagged representations and exact expected occurrences.
    pub representations: Vec<IdentityRepresentation>,
}

/// Logical fixture principal. IDs are fixture-local labels, not hardware IDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixturePrincipal {
    /// One physical receiver represented by a synthetic receiver identity.
    Receiver {
        /// Fixture-local logical ID.
        id: String,
    },
    /// One device at a typed semantic profile route.
    Device {
        /// Fixture-local logical ID.
        id: String,
        /// Profile route where this principal's identity must occur.
        route: FixtureDeviceRoute,
    },
}

impl FixturePrincipal {
    /// Return this principal's fixture-local ID.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Receiver { id } | Self::Device { id, .. } => id,
        }
    }
}

/// Typed route locating a logical device in a profile.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureDeviceRoute {
    /// Device paired to a Bolt receiver.
    Bolt {
        /// Fixture-local receiver principal ID.
        receiver: String,
        /// Pairing slot.
        slot: u8,
    },
    /// Device paired to a Unifying-protocol receiver.
    Unifying {
        /// Fixture-local receiver principal ID.
        receiver: String,
        /// Pairing slot.
        slot: u8,
    },
    /// Direct HID++ device.
    Direct {
        /// HID vendor ID.
        vendor_id: u16,
        /// HID product ID.
        product_id: u16,
    },
    /// Standalone raw-HID device.
    RawHid {
        /// HID vendor ID.
        vendor_id: u16,
        /// HID product ID.
        product_id: u16,
        /// HID usage page.
        usage_page: u16,
        /// HID usage ID.
        usage_id: u16,
    },
}

impl FixtureDeviceRoute {
    pub(super) fn receiver(&self) -> Option<&str> {
        match self {
            Self::Bolt { receiver, .. } | Self::Unifying { receiver, .. } => Some(receiver),
            Self::Direct { .. } | Self::RawHid { .. } => None,
        }
    }
}

/// Strongly tagged identity values and their exact expected occurrences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityRepresentation {
    /// Fixed 16-byte tagged ASCII Bolt receiver UID.
    BoltReceiverUid {
        /// Canonical synthetic UID.
        value: String,
        /// Profile and cassette occurrences.
        occurrences: Vec<IdentityOccurrence>,
    },
    /// One Unifying binary serial and its uppercase-hex profile route relation.
    UnifyingReceiverSerial {
        /// Tagged four-byte receiver serial used in protocol traffic.
        value: [u8; 4],
        /// Uppercase hexadecimal route derived exactly from `value`.
        profile_route: String,
        /// Binary protocol occurrences.
        binary_occurrences: Vec<IdentityOccurrence>,
        /// Profile route-string occurrences.
        route_occurrences: Vec<IdentityOccurrence>,
    },
    /// Tagged four-byte HID++ device unit ID.
    DeviceUnitId {
        /// Canonical synthetic unit ID.
        value: [u8; 4],
        /// Profile and cassette occurrences.
        occurrences: Vec<IdentityOccurrence>,
    },
    /// Fixed 12-byte tagged ASCII DeviceInformation serial.
    DeviceSerialNumber {
        /// Canonical synthetic serial.
        value: String,
        /// Profile and cassette occurrences.
        occurrences: Vec<IdentityOccurrence>,
    },
    /// Explicit tagged raw-HID profile identity.
    RawHidProfileIdentity {
        /// Canonical synthetic profile identity.
        value: String,
        /// Profile-only occurrences.
        occurrences: Vec<IdentityOccurrence>,
    },
}

/// Exact expected count of an identity at one typed asset location.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityOccurrence {
    /// Typed profile or cassette location.
    pub location: IdentityLocation,
    /// Exact nonzero occurrence count.
    pub count: u32,
}

/// Asset location where one identity representation must occur.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "asset", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityLocation {
    /// Semantic profile field.
    Profile {
        /// Typed profile identity field.
        field: ProfileIdentityField,
    },
    /// Named cassette on its declared logical channel.
    Cassette {
        /// Case name from [`FixtureManifest::cases`].
        case: String,
        /// Exact logical channel expected for the case and cassette.
        channel: String,
    },
}

/// Identity-bearing semantic profile field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileIdentityField {
    /// `DeviceInventory.receiver.unique_id`.
    ReceiverIdentity,
    /// Receiver UID carried by a Bolt/Unifying settings route.
    ReceiverRoute,
    /// `DeviceModelInfo.unit_id` or `StandaloneDevice.unit_id`.
    DeviceUnitId,
    /// DeviceInformation or standalone serial field.
    DeviceSerialNumber,
    /// `StandaloneDevice.address.identity`.
    RawHidAddressIdentity,
    /// Raw-HID settings-route identity.
    RawHidRouteIdentity,
}

impl FixtureManifest {
    /// Validate schema, logical relationships, synthetic values, and occurrence declarations.
    pub fn validate(&self) -> Result<(), FixtureError> {
        if self.schema_version != FIXTURE_SCHEMA_VERSION {
            return Err(FixtureError::UnsupportedSchema {
                asset: "fixture manifest",
                actual: self.schema_version,
                supported: FIXTURE_SCHEMA_VERSION,
            });
        }
        validate_name("id", &self.id)?;
        validate_name("profile_id", &self.profile_id)?;

        let mut principals = BTreeMap::new();
        for entry in &self.identity_ledger {
            validate_name("principal id", entry.principal.id())?;
            if principals
                .insert(entry.principal.id(), &entry.principal)
                .is_some()
            {
                return invalid(format!("duplicate principal id {}", entry.principal.id()));
            }
            if entry.representations.is_empty() {
                return invalid(format!(
                    "principal {} has no identity representations",
                    entry.principal.id()
                ));
            }
        }
        validate_principal_routes(&principals)?;
        let cases = validate_cases(&self.cases, &principals)?;
        validate_ledger(&self.identity_ledger, &principals, &cases)
    }
}

fn validate_principal_routes(
    principals: &BTreeMap<&str, &FixturePrincipal>,
) -> Result<(), FixtureError> {
    let mut routes = BTreeSet::new();
    for principal in principals.values() {
        let FixturePrincipal::Device { id, route } = principal else {
            continue;
        };
        if let Some(receiver) = route.receiver()
            && !matches!(
                principals.get(receiver),
                Some(FixturePrincipal::Receiver { .. })
            )
        {
            return invalid(format!(
                "device principal {id} names unknown receiver {receiver}"
            ));
        }
        if matches!(route, FixtureDeviceRoute::Bolt { slot, .. } | FixtureDeviceRoute::Unifying { slot, .. } if !(1..=6).contains(slot))
        {
            return invalid(format!("device principal {id} has invalid receiver slot"));
        }
        if !routes.insert(route) {
            return invalid(format!(
                "device profile route for principal {id} is duplicated"
            ));
        }
    }
    Ok(())
}

fn validate_cases<'a>(
    case_list: &'a [FixtureCase],
    principals: &BTreeMap<&str, &FixturePrincipal>,
) -> Result<BTreeMap<&'a str, &'a FixtureCase>, FixtureError> {
    let mut cases = BTreeMap::new();
    for case in case_list {
        validate_name("case name", &case.name)?;
        validate_name("case channel", &case.channel)?;
        if cases.insert(case.name.as_str(), case).is_some() {
            return invalid(format!("duplicate fixture case {}", case.name));
        }
        let valid = match &case.relationship {
            FixtureCaseRelationship::Receiver { receiver } => {
                matches!(
                    principals.get(receiver.as_str()),
                    Some(FixturePrincipal::Receiver { .. })
                )
            }
            FixtureCaseRelationship::Device { device } => {
                matches!(
                    principals.get(device.as_str()),
                    Some(FixturePrincipal::Device { .. })
                )
            }
        };
        if !valid {
            return invalid(format!(
                "case {} names the wrong or unknown principal",
                case.name
            ));
        }
    }
    Ok(cases)
}

fn validate_ledger(
    ledger: &[IdentityLedgerEntry],
    principals: &BTreeMap<&str, &FixturePrincipal>,
    cases: &BTreeMap<&str, &FixtureCase>,
) -> Result<(), FixtureError> {
    let mut values = BTreeSet::new();
    for entry in ledger {
        let mut kinds = BTreeSet::new();
        if matches!(entry.principal, FixturePrincipal::Receiver { .. })
            && entry.representations.len() != 1
        {
            return invalid(format!(
                "receiver principal {} must have exactly one receiver identity",
                entry.principal.id()
            ));
        }
        for representation in &entry.representations {
            let kind = representation.kind();
            if !kinds.insert(kind) {
                return invalid(format!(
                    "principal {} repeats representation {kind:?}",
                    entry.principal.id()
                ));
            }
            representation.validate_value()?;
            for value in representation.value_keys() {
                if !values.insert(value) {
                    return invalid("synthetic identity value is assigned more than once");
                }
            }
            validate_representation_owner(&entry.principal, representation)?;
            let occurrences = representation.occurrences();
            if occurrences.is_empty() {
                return invalid(format!(
                    "principal {} representation {kind:?} has no occurrences",
                    entry.principal.id()
                ));
            }
            let mut locations = BTreeSet::new();
            for occurrence in occurrences {
                if !locations.insert((occurrence.1, &occurrence.0.location)) {
                    return invalid(format!(
                        "principal {} repeats an identity occurrence location",
                        entry.principal.id()
                    ));
                }
                validate_occurrence(entry.principal.id(), occurrence, principals, cases)?;
            }
        }
    }
    Ok(())
}

impl IdentityRepresentation {
    pub(super) const fn kind(&self) -> SyntheticIdentityKind {
        match self {
            Self::BoltReceiverUid { .. } => SyntheticIdentityKind::BoltReceiverUid,
            Self::UnifyingReceiverSerial { .. } => SyntheticIdentityKind::UnifyingReceiverSerial,
            Self::DeviceUnitId { .. } => SyntheticIdentityKind::DeviceUnitId,
            Self::DeviceSerialNumber { .. } => SyntheticIdentityKind::DeviceSerialNumber,
            Self::RawHidProfileIdentity { .. } => SyntheticIdentityKind::RawHidProfileIdentity,
        }
    }

    fn validate_value(&self) -> Result<(), FixtureError> {
        let valid = match self {
            Self::BoltReceiverUid { value, .. } => {
                classify_synthetic_profile_identity(SyntheticIdentityKind::BoltReceiverUid, value)
            }
            Self::UnifyingReceiverSerial {
                value,
                profile_route,
                ..
            } => {
                if let Err(error) = classify_synthetic_identity_bytes(
                    SyntheticIdentityKind::UnifyingReceiverSerial,
                    value,
                ) {
                    return invalid(error.to_string());
                }
                if profile_route != &unifying_receiver_route(*value) {
                    return invalid("Unifying binary serial and uppercase profile route disagree");
                }
                classify_synthetic_profile_identity(
                    SyntheticIdentityKind::UnifyingReceiverRoute,
                    profile_route,
                )
            }
            Self::DeviceUnitId { value, .. } => {
                classify_synthetic_identity_bytes(SyntheticIdentityKind::DeviceUnitId, value)
            }
            Self::DeviceSerialNumber { value, .. } => classify_synthetic_profile_identity(
                SyntheticIdentityKind::DeviceSerialNumber,
                value,
            ),
            Self::RawHidProfileIdentity { value, .. } => classify_synthetic_profile_identity(
                SyntheticIdentityKind::RawHidProfileIdentity,
                value,
            ),
        };
        valid
            .map(|_| ())
            .map_err(|error| FixtureError::invalid("fixture manifest", error.to_string()))
    }

    pub(super) fn value_keys(&self) -> Vec<(SyntheticIdentityKind, Vec<u8>)> {
        match self {
            Self::BoltReceiverUid { value, .. } => {
                vec![(
                    SyntheticIdentityKind::BoltReceiverUid,
                    value.as_bytes().to_vec(),
                )]
            }
            Self::UnifyingReceiverSerial {
                value,
                profile_route,
                ..
            } => vec![
                (
                    SyntheticIdentityKind::UnifyingReceiverSerial,
                    value.to_vec(),
                ),
                (
                    SyntheticIdentityKind::UnifyingReceiverRoute,
                    profile_route.as_bytes().to_vec(),
                ),
            ],
            Self::DeviceUnitId { value, .. } => {
                vec![(SyntheticIdentityKind::DeviceUnitId, value.to_vec())]
            }
            Self::DeviceSerialNumber { value, .. } => vec![(
                SyntheticIdentityKind::DeviceSerialNumber,
                value.as_bytes().to_vec(),
            )],
            Self::RawHidProfileIdentity { value, .. } => vec![(
                SyntheticIdentityKind::RawHidProfileIdentity,
                value.as_bytes().to_vec(),
            )],
        }
    }

    pub(super) fn occurrences(&self) -> Vec<(&IdentityOccurrence, SyntheticIdentityKind)> {
        match self {
            Self::BoltReceiverUid { occurrences, .. }
            | Self::DeviceUnitId { occurrences, .. }
            | Self::DeviceSerialNumber { occurrences, .. }
            | Self::RawHidProfileIdentity { occurrences, .. } => occurrences
                .iter()
                .map(|occurrence| (occurrence, self.kind()))
                .collect(),
            Self::UnifyingReceiverSerial {
                binary_occurrences,
                route_occurrences,
                ..
            } => {
                binary_occurrences
                    .iter()
                    .map(|occurrence| (occurrence, SyntheticIdentityKind::UnifyingReceiverSerial))
                    .chain(route_occurrences.iter().map(|occurrence| {
                        (occurrence, SyntheticIdentityKind::UnifyingReceiverRoute)
                    }))
                    .collect()
            }
        }
    }
}

fn validate_representation_owner(
    principal: &FixturePrincipal,
    representation: &IdentityRepresentation,
) -> Result<(), FixtureError> {
    let mut valid = matches!(
        (principal, representation),
        (
            FixturePrincipal::Receiver { .. },
            IdentityRepresentation::BoltReceiverUid { .. }
                | IdentityRepresentation::UnifyingReceiverSerial { .. }
        ) | (
            FixturePrincipal::Device { .. },
            IdentityRepresentation::DeviceUnitId { .. }
                | IdentityRepresentation::DeviceSerialNumber { .. }
                | IdentityRepresentation::RawHidProfileIdentity { .. }
        )
    );
    if matches!(
        representation,
        IdentityRepresentation::RawHidProfileIdentity { .. }
    ) {
        valid = matches!(
            principal,
            FixturePrincipal::Device {
                route: FixtureDeviceRoute::RawHid { .. },
                ..
            }
        );
    }
    valid.then_some(()).ok_or_else(|| {
        FixtureError::invalid(
            "fixture manifest",
            format!(
                "principal {} cannot own representation {:?}",
                principal.id(),
                representation.kind()
            ),
        )
    })
}

fn validate_occurrence(
    principal: &str,
    (occurrence, value_kind): (&IdentityOccurrence, SyntheticIdentityKind),
    principals: &BTreeMap<&str, &FixturePrincipal>,
    cases: &BTreeMap<&str, &FixtureCase>,
) -> Result<(), FixtureError> {
    if occurrence.count == 0 {
        return invalid("identity occurrence count must be nonzero");
    }
    match &occurrence.location {
        IdentityLocation::Profile { field } => {
            let valid = matches!(
                (value_kind, field),
                (
                    SyntheticIdentityKind::BoltReceiverUid
                        | SyntheticIdentityKind::UnifyingReceiverRoute,
                    ProfileIdentityField::ReceiverIdentity | ProfileIdentityField::ReceiverRoute
                ) | (
                    SyntheticIdentityKind::DeviceUnitId,
                    ProfileIdentityField::DeviceUnitId
                ) | (
                    SyntheticIdentityKind::DeviceSerialNumber,
                    ProfileIdentityField::DeviceSerialNumber
                ) | (
                    SyntheticIdentityKind::RawHidProfileIdentity,
                    ProfileIdentityField::RawHidAddressIdentity
                        | ProfileIdentityField::RawHidRouteIdentity
                )
            );
            if !valid {
                return invalid(format!(
                    "{value_kind:?} cannot occur in profile field {field:?}"
                ));
            }
        }
        IdentityLocation::Cassette { case, channel } => {
            if value_kind == SyntheticIdentityKind::UnifyingReceiverRoute
                || value_kind == SyntheticIdentityKind::RawHidProfileIdentity
            {
                return invalid(format!("{value_kind:?} cannot occur in a HID++ cassette"));
            }
            let Some(case_definition) = cases.get(case.as_str()) else {
                return invalid(format!("identity occurrence names unknown case {case}"));
            };
            if case_definition.channel != *channel {
                return invalid(format!(
                    "identity occurrence has wrong channel for case {case}"
                ));
            }
            if !case_relates_principal(case_definition, principal, principals) {
                return invalid(format!(
                    "case {case} is unrelated to identity principal {principal}"
                ));
            }
        }
    }
    Ok(())
}

fn case_relates_principal(
    case: &FixtureCase,
    principal: &str,
    principals: &BTreeMap<&str, &FixturePrincipal>,
) -> bool {
    let receiver_for = |device: &str| match principals.get(device) {
        Some(FixturePrincipal::Device { route, .. }) => route.receiver(),
        _ => None,
    };
    match &case.relationship {
        FixtureCaseRelationship::Receiver { receiver } => {
            principal == receiver || receiver_for(principal) == Some(receiver)
        }
        FixtureCaseRelationship::Device { device } => {
            principal == device
                || receiver_for(device).is_some_and(|receiver| receiver == principal)
        }
    }
}

fn validate_name(field: &str, value: &str) -> Result<(), FixtureError> {
    if value.trim().is_empty() {
        invalid(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, FixtureError> {
    Err(FixtureError::invalid("fixture manifest", message))
}
