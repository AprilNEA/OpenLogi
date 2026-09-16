//! Deterministic fixture-manifest generation from already sanitized assets.

use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::device::DeviceModelInfo;
use openlogi_core::hid::{DeviceRoute, speaks_unifying_protocol};

use super::{
    DeviceProfile, FIXTURE_SCHEMA_VERSION, FixtureCase, FixtureCaseRelationship,
    FixtureDeviceRoute, FixtureError, FixtureManifest, FixturePrincipal, HidCassette,
    IdentityLedgerEntry, IdentityRepresentation, SyntheticIdentityKind,
    classify_synthetic_identity_bytes, classify_synthetic_profile_identity,
    generate_synthetic_identity,
};

/// Relates one named cassette to the sanitized semantic-profile route it exercises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixtureCaseBinding {
    /// Cassette name. This must name exactly one supplied cassette.
    pub name: String,
    /// Sanitized profile route for the device exercised by the cassette.
    pub route: DeviceRoute,
}

impl FixtureManifest {
    /// Generate an exact identity ledger and case relationships from sanitized assets.
    ///
    /// The profile and cassette contents remain the source of truth: identity
    /// occurrences are re-extracted from those assets, rather than copied from
    /// capture audit output. Every cassette must have one binding to a device
    /// principal present in the profile.
    pub fn from_assets(
        id: String,
        profile: &DeviceProfile,
        cassettes: &[HidCassette],
        bindings: &[FixtureCaseBinding],
    ) -> Result<Self, FixtureError> {
        profile.validate()?;
        let mut manifest = Self {
            schema_version: FIXTURE_SCHEMA_VERSION,
            id,
            profile_id: profile.id.clone(),
            cases: Vec::new(),
            identity_ledger: build_profile_ledger(profile)?,
        };
        manifest.cases = build_cases(&manifest.identity_ledger, cassettes, bindings)?;
        super::verify::populate_exact_occurrences(&mut manifest, profile, cassettes)?;
        manifest.verify(profile, cassettes)?;
        Ok(manifest)
    }
}

fn build_profile_ledger(profile: &DeviceProfile) -> Result<Vec<IdentityLedgerEntry>, FixtureError> {
    let mut ledger = Vec::new();
    let mut receiver_number = 0usize;
    let mut device_number = 0usize;

    for inventory in &profile.inventories {
        let receiver = if let Some(identity) = inventory.receiver.unique_id.as_deref() {
            receiver_number = receiver_number.saturating_add(1);
            let id = format!("receiver-{receiver_number}");
            let representation = receiver_representation(inventory.receiver.product_id, identity)?;
            ledger.push(IdentityLedgerEntry {
                principal: FixturePrincipal::Receiver { id: id.clone() },
                representations: vec![representation],
            });
            Some(id)
        } else {
            None
        };

        for device in &inventory.paired {
            let Some(model) = device.model_info.as_ref() else {
                continue;
            };
            let representations = device_representations(model)?;
            if representations.is_empty() {
                continue;
            }
            device_number = device_number.saturating_add(1);
            let route = match &receiver {
                Some(receiver) if speaks_unifying_protocol(inventory.receiver.product_id) => {
                    FixtureDeviceRoute::Unifying {
                        receiver: receiver.clone(),
                        slot: device.slot,
                    }
                }
                Some(receiver) => FixtureDeviceRoute::Bolt {
                    receiver: receiver.clone(),
                    slot: device.slot,
                },
                None => FixtureDeviceRoute::Direct {
                    vendor_id: inventory.receiver.vendor_id,
                    product_id: inventory.receiver.product_id,
                },
            };
            ledger.push(IdentityLedgerEntry {
                principal: FixturePrincipal::Device {
                    id: format!("device-{device_number}"),
                    route,
                },
                representations,
            });
        }
    }

    for device in &profile.standalone {
        let ordinal = classify_synthetic_profile_identity(
            SyntheticIdentityKind::RawHidProfileIdentity,
            &device.address.identity,
        )
        .map_err(|error| manifest_error(error.to_string()))?;
        let mut representations = vec![IdentityRepresentation::RawHidProfileIdentity {
            value: device.address.identity.clone(),
            occurrences: Vec::new(),
        }];
        add_device_identity_representations(
            &mut representations,
            device.unit_id,
            device.serial_number.as_deref(),
        )?;
        let generated =
            generate_synthetic_identity(SyntheticIdentityKind::RawHidProfileIdentity, ordinal);
        if generated.as_profile_str() != Some(device.address.identity.as_str()) {
            return invalid("raw-HID identity is not canonical");
        }
        device_number = device_number.saturating_add(1);
        ledger.push(IdentityLedgerEntry {
            principal: FixturePrincipal::Device {
                id: format!("device-{device_number}"),
                route: FixtureDeviceRoute::RawHid {
                    vendor_id: device.address.vendor_id,
                    product_id: device.address.product_id,
                    usage_page: device.address.usage_page,
                    usage_id: device.address.usage_id,
                },
            },
            representations,
        });
    }

    Ok(ledger)
}

fn receiver_representation(
    product_id: u16,
    identity: &str,
) -> Result<IdentityRepresentation, FixtureError> {
    if speaks_unifying_protocol(product_id) {
        let ordinal = classify_synthetic_profile_identity(
            SyntheticIdentityKind::UnifyingReceiverRoute,
            identity,
        )
        .map_err(|error| manifest_error(error.to_string()))?;
        let generated =
            generate_synthetic_identity(SyntheticIdentityKind::UnifyingReceiverSerial, ordinal);
        let value: [u8; 4] = generated
            .as_bytes()
            .and_then(|value| value.try_into().ok())
            .ok_or_else(|| manifest_error("could not derive Unifying receiver serial"))?;
        Ok(IdentityRepresentation::UnifyingReceiverSerial {
            value,
            profile_route: identity.to_string(),
            binary_occurrences: Vec::new(),
            route_occurrences: Vec::new(),
        })
    } else {
        classify_synthetic_profile_identity(SyntheticIdentityKind::BoltReceiverUid, identity)
            .map_err(|error| manifest_error(error.to_string()))?;
        Ok(IdentityRepresentation::BoltReceiverUid {
            value: identity.to_string(),
            occurrences: Vec::new(),
        })
    }
}

fn device_representations(
    model: &DeviceModelInfo,
) -> Result<Vec<IdentityRepresentation>, FixtureError> {
    let mut representations = Vec::new();
    add_device_identity_representations(
        &mut representations,
        model.unit_id,
        model.serial_number.as_deref(),
    )?;
    Ok(representations)
}

fn add_device_identity_representations(
    representations: &mut Vec<IdentityRepresentation>,
    unit_id: [u8; 4],
    serial_number: Option<&str>,
) -> Result<(), FixtureError> {
    if unit_id != [0; 4] {
        classify_synthetic_identity_bytes(SyntheticIdentityKind::DeviceUnitId, &unit_id)
            .map_err(|error| manifest_error(error.to_string()))?;
        representations.push(IdentityRepresentation::DeviceUnitId {
            value: unit_id,
            occurrences: Vec::new(),
        });
    }
    if let Some(serial_number) = serial_number {
        classify_synthetic_profile_identity(
            SyntheticIdentityKind::DeviceSerialNumber,
            serial_number,
        )
        .map_err(|error| manifest_error(error.to_string()))?;
        representations.push(IdentityRepresentation::DeviceSerialNumber {
            value: serial_number.to_string(),
            occurrences: Vec::new(),
        });
    }
    Ok(())
}

fn build_cases(
    ledger: &[IdentityLedgerEntry],
    cassettes: &[HidCassette],
    bindings: &[FixtureCaseBinding],
) -> Result<Vec<FixtureCase>, FixtureError> {
    if cassettes.len() != bindings.len() {
        return invalid("every cassette must have exactly one route binding");
    }
    let mut binding_map = BTreeMap::new();
    for binding in bindings {
        if binding_map.insert(binding.name.as_str(), binding).is_some() {
            return invalid(format!("duplicate case binding {}", binding.name));
        }
    }
    let mut cassette_names = BTreeSet::new();
    cassettes
        .iter()
        .map(|cassette| {
            if !cassette_names.insert(cassette.name.as_str()) {
                return invalid(format!("duplicate cassette {}", cassette.name));
            }
            let binding = binding_map.get(cassette.name.as_str()).ok_or_else(|| {
                manifest_error(format!("cassette {} has no binding", cassette.name))
            })?;
            let device = resolve_profile_route(ledger, &binding.route)?;
            Ok(FixtureCase {
                name: cassette.name.clone(),
                channel: cassette.channel.clone(),
                relationship: FixtureCaseRelationship::Device {
                    device: device.to_string(),
                },
            })
        })
        .collect()
}

fn resolve_profile_route<'a>(
    ledger: &'a [IdentityLedgerEntry],
    route: &DeviceRoute,
) -> Result<&'a str, FixtureError> {
    let receiver = match route {
        DeviceRoute::Bolt { receiver_uid, .. } => Some((
            SyntheticIdentityKind::BoltReceiverUid,
            receiver_uid.as_bytes(),
        )),
        DeviceRoute::Unifying { receiver_uid, .. } => Some((
            SyntheticIdentityKind::UnifyingReceiverRoute,
            receiver_uid.as_bytes(),
        )),
        DeviceRoute::Direct { .. } | DeviceRoute::RawHid { .. } => None,
    };
    let receiver_id = receiver.and_then(|(kind, value)| {
        ledger.iter().find_map(|entry| {
            entry
                .representations
                .iter()
                .flat_map(IdentityRepresentation::value_keys)
                .any(|key| key.0 == kind && key.1 == value)
                .then_some(entry.principal.id())
        })
    });
    let fixture_route = match route {
        DeviceRoute::Bolt { slot, .. } => FixtureDeviceRoute::Bolt {
            receiver: receiver_id
                .ok_or_else(|| manifest_error("Bolt route has no receiver principal"))?
                .to_string(),
            slot: *slot,
        },
        DeviceRoute::Unifying { slot, .. } => FixtureDeviceRoute::Unifying {
            receiver: receiver_id
                .ok_or_else(|| manifest_error("Unifying route has no receiver principal"))?
                .to_string(),
            slot: *slot,
        },
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => FixtureDeviceRoute::Direct {
            vendor_id: *vendor_id,
            product_id: *product_id,
        },
        DeviceRoute::RawHid {
            vendor_id,
            product_id,
            usage_page,
            usage_id,
            ..
        } => FixtureDeviceRoute::RawHid {
            vendor_id: *vendor_id,
            product_id: *product_id,
            usage_page: *usage_page,
            usage_id: *usage_id,
        },
    };
    let mut matches = ledger.iter().filter_map(|entry| match &entry.principal {
        FixturePrincipal::Device { id, route } if route == &fixture_route => Some(id.as_str()),
        FixturePrincipal::Receiver { .. } | FixturePrincipal::Device { .. } => None,
    });
    let principal = matches
        .next()
        .ok_or_else(|| manifest_error("case route has no identity-bearing device principal"))?;
    if matches.next().is_some() {
        return invalid("case route resolves to more than one device principal");
    }
    Ok(principal)
}

fn manifest_error(message: impl Into<String>) -> FixtureError {
    FixtureError::invalid("fixture manifest generation", message)
}

fn invalid<T>(message: impl Into<String>) -> Result<T, FixtureError> {
    Err(manifest_error(message))
}
