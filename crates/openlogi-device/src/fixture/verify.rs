use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::hid::{DeviceRoute, speaks_unifying_protocol};

use super::{
    DeviceProfile, FixtureDeviceRoute, FixtureError, FixtureManifest, FixturePrincipal,
    HidCassette, IdentityLocation, ProfileIdentityField, ProtocolIdentityExtractor,
    SyntheticIdentityKind, classify_synthetic_identity_bytes, classify_synthetic_profile_identity,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CountKey {
    principal: String,
    kind: SyntheticIdentityKind,
    location: IdentityLocation,
}

struct LedgerIndex<'a> {
    values: BTreeMap<(SyntheticIdentityKind, Vec<u8>), &'a str>,
    principals: BTreeMap<&'a str, &'a FixturePrincipal>,
    expected: BTreeMap<CountKey, u32>,
}

impl FixtureManifest {
    /// Verify a semantic profile and complete named cassette set against this ledger.
    ///
    /// Verification re-extracts every supported identity field, classifies its
    /// value under the shared synthetic policy, and compares exact principal,
    /// location, relationship, and count evidence. It trusts no capture audit or
    /// sanitization declaration.
    pub fn verify(
        &self,
        profile: &DeviceProfile,
        cassettes: &[HidCassette],
    ) -> Result<(), FixtureError> {
        self.validate()?;
        profile.validate()?;
        if profile.id != self.profile_id {
            return invalid(format!(
                "manifest profile_id {} does not match profile {}",
                self.profile_id, profile.id
            ));
        }
        let cassette_map = self.validate_cassettes(cassettes)?;
        let ledger = LedgerIndex::new(self)?;
        let mut observed = BTreeMap::new();
        verify_profile(profile, &ledger, &mut observed)?;
        verify_cassettes(&cassette_map, &ledger, &mut observed)?;
        compare_counts(&ledger.expected, &observed)
    }

    fn validate_cassettes<'a>(
        &self,
        cassettes: &'a [HidCassette],
    ) -> Result<BTreeMap<&'a str, &'a HidCassette>, FixtureError> {
        let cases: BTreeMap<_, _> = self
            .cases
            .iter()
            .map(|case| (case.name.as_str(), case))
            .collect();
        let mut found = BTreeMap::new();
        for cassette in cassettes {
            cassette.validate()?;
            let Some(case) = cases.get(cassette.name.as_str()) else {
                return invalid(format!("extra cassette {}", cassette.name));
            };
            if cassette.channel != case.channel {
                return invalid(format!(
                    "cassette {} uses channel {}, expected {}",
                    cassette.name, cassette.channel, case.channel
                ));
            }
            if found.insert(cassette.name.as_str(), cassette).is_some() {
                return invalid(format!("duplicate cassette {}", cassette.name));
            }
        }
        if let Some(missing) = self
            .cases
            .iter()
            .find(|case| !found.contains_key(case.name.as_str()))
        {
            return invalid(format!("missing cassette {}", missing.name));
        }
        Ok(found)
    }
}

impl<'a> LedgerIndex<'a> {
    fn new(manifest: &'a FixtureManifest) -> Result<Self, FixtureError> {
        let principals: BTreeMap<_, _> = manifest
            .identity_ledger
            .iter()
            .map(|entry| (entry.principal.id(), &entry.principal))
            .collect();
        let mut values = BTreeMap::new();
        let mut expected = BTreeMap::new();
        for entry in &manifest.identity_ledger {
            for representation in &entry.representations {
                for value in representation.value_keys() {
                    values.insert(value, entry.principal.id());
                }
                for (occurrence, kind) in representation.occurrences() {
                    let key = CountKey {
                        principal: entry.principal.id().to_string(),
                        kind,
                        location: occurrence.location.clone(),
                    };
                    if expected.insert(key, occurrence.count).is_some() {
                        return invalid(format!(
                            "principal {} repeats an identity occurrence location",
                            entry.principal.id()
                        ));
                    }
                }
            }
        }
        Ok(Self {
            values,
            principals,
            expected,
        })
    }

    fn resolve_bytes(
        &self,
        kind: SyntheticIdentityKind,
        value: &[u8],
    ) -> Result<&str, FixtureError> {
        classify_synthetic_identity_bytes(kind, value)
            .map_err(|error| FixtureError::invalid("fixture verification", error.to_string()))?;
        self.values
            .get(&(kind, value.to_vec()))
            .copied()
            .ok_or_else(|| {
                FixtureError::invalid(
                    "fixture verification",
                    format!("extra synthetic {kind:?} value not declared by the ledger"),
                )
            })
    }

    fn resolve_profile(
        &self,
        kind: SyntheticIdentityKind,
        value: &str,
    ) -> Result<&str, FixtureError> {
        classify_synthetic_profile_identity(kind, value)
            .map_err(|error| FixtureError::invalid("fixture verification", error.to_string()))?;
        self.values
            .get(&(kind, value.as_bytes().to_vec()))
            .copied()
            .ok_or_else(|| {
                FixtureError::invalid(
                    "fixture verification",
                    format!("extra synthetic {kind:?} value not declared by the ledger"),
                )
            })
    }

    fn device_route(&self, principal: &str) -> Option<&FixtureDeviceRoute> {
        match self.principals.get(principal) {
            Some(FixturePrincipal::Device { route, .. }) => Some(route),
            _ => None,
        }
    }
}

fn verify_profile(
    profile: &DeviceProfile,
    ledger: &LedgerIndex<'_>,
    observed: &mut BTreeMap<CountKey, u32>,
) -> Result<(), FixtureError> {
    let mut seen_principals = BTreeSet::new();
    for inventory in &profile.inventories {
        let receiver = if let Some(identity) = inventory.receiver.unique_id.as_deref() {
            let kind = if speaks_unifying_protocol(inventory.receiver.product_id) {
                SyntheticIdentityKind::UnifyingReceiverRoute
            } else {
                SyntheticIdentityKind::BoltReceiverUid
            };
            let principal = ledger.resolve_profile(kind, identity)?;
            observe_profile(
                observed,
                principal,
                kind,
                ProfileIdentityField::ReceiverIdentity,
            );
            seen_principals.insert(principal.to_string());
            Some((principal, kind))
        } else {
            None
        };

        for device in &inventory.paired {
            let Some(model) = &device.model_info else {
                continue;
            };
            let route = match receiver {
                Some((receiver, SyntheticIdentityKind::UnifyingReceiverRoute)) => {
                    FixtureDeviceRoute::Unifying {
                        receiver: receiver.to_string(),
                        slot: device.slot,
                    }
                }
                Some((receiver, SyntheticIdentityKind::BoltReceiverUid)) => {
                    FixtureDeviceRoute::Bolt {
                        receiver: receiver.to_string(),
                        slot: device.slot,
                    }
                }
                Some(_) => return invalid("receiver resolved to an unsupported profile kind"),
                None => FixtureDeviceRoute::Direct {
                    vendor_id: inventory.receiver.vendor_id,
                    product_id: inventory.receiver.product_id,
                },
            };
            let principal = resolve_device_route(ledger, &route)?;
            seen_principals.insert(principal.to_string());
            observe_device_model(model, principal, ledger, observed)?;
        }
    }

    for device in &profile.standalone {
        let principal = ledger.resolve_profile(
            SyntheticIdentityKind::RawHidProfileIdentity,
            &device.address.identity,
        )?;
        let route = FixtureDeviceRoute::RawHid {
            vendor_id: device.address.vendor_id,
            product_id: device.address.product_id,
            usage_page: device.address.usage_page,
            usage_id: device.address.usage_id,
        };
        require_device_route(ledger, principal, &route)?;
        seen_principals.insert(principal.to_string());
        observe_profile(
            observed,
            principal,
            SyntheticIdentityKind::RawHidProfileIdentity,
            ProfileIdentityField::RawHidAddressIdentity,
        );
        if device.unit_id != [0; 4] {
            observe_bytes_for_principal(
                observed,
                ledger,
                principal,
                SyntheticIdentityKind::DeviceUnitId,
                &device.unit_id,
                ProfileIdentityField::DeviceUnitId,
            )?;
        }
        if let Some(serial) = device.serial_number.as_deref() {
            observe_profile_for_principal(
                observed,
                ledger,
                principal,
                SyntheticIdentityKind::DeviceSerialNumber,
                serial,
                ProfileIdentityField::DeviceSerialNumber,
            )?;
        }
    }

    verify_setting_routes(profile, ledger, observed)?;
    if let Some(missing) = ledger
        .principals
        .keys()
        .find(|principal| !seen_principals.contains(**principal))
    {
        return invalid(format!("principal {missing} has no matching profile route"));
    }
    Ok(())
}

fn observe_device_model(
    model: &openlogi_core::device::DeviceModelInfo,
    principal: &str,
    ledger: &LedgerIndex<'_>,
    observed: &mut BTreeMap<CountKey, u32>,
) -> Result<(), FixtureError> {
    if model.unit_id != [0; 4] {
        observe_bytes_for_principal(
            observed,
            ledger,
            principal,
            SyntheticIdentityKind::DeviceUnitId,
            &model.unit_id,
            ProfileIdentityField::DeviceUnitId,
        )?;
    }
    if let Some(serial) = model.serial_number.as_deref() {
        observe_profile_for_principal(
            observed,
            ledger,
            principal,
            SyntheticIdentityKind::DeviceSerialNumber,
            serial,
            ProfileIdentityField::DeviceSerialNumber,
        )?;
    }
    Ok(())
}

fn verify_setting_routes(
    profile: &DeviceProfile,
    ledger: &LedgerIndex<'_>,
    observed: &mut BTreeMap<CountKey, u32>,
) -> Result<(), FixtureError> {
    for settings in &profile.settings {
        match &settings.route {
            DeviceRoute::Bolt { receiver_uid, .. } => {
                let principal =
                    ledger.resolve_profile(SyntheticIdentityKind::BoltReceiverUid, receiver_uid)?;
                observe_profile(
                    observed,
                    principal,
                    SyntheticIdentityKind::BoltReceiverUid,
                    ProfileIdentityField::ReceiverRoute,
                );
            }
            DeviceRoute::Unifying { receiver_uid, .. } => {
                let principal = ledger
                    .resolve_profile(SyntheticIdentityKind::UnifyingReceiverRoute, receiver_uid)?;
                observe_profile(
                    observed,
                    principal,
                    SyntheticIdentityKind::UnifyingReceiverRoute,
                    ProfileIdentityField::ReceiverRoute,
                );
            }
            DeviceRoute::RawHid {
                vendor_id,
                product_id,
                usage_page,
                usage_id,
                identity,
            } => {
                let principal = ledger
                    .resolve_profile(SyntheticIdentityKind::RawHidProfileIdentity, identity)?;
                require_device_route(
                    ledger,
                    principal,
                    &FixtureDeviceRoute::RawHid {
                        vendor_id: *vendor_id,
                        product_id: *product_id,
                        usage_page: *usage_page,
                        usage_id: *usage_id,
                    },
                )?;
                observe_profile(
                    observed,
                    principal,
                    SyntheticIdentityKind::RawHidProfileIdentity,
                    ProfileIdentityField::RawHidRouteIdentity,
                );
            }
            DeviceRoute::Direct { .. } => {}
        }
    }
    Ok(())
}

fn verify_cassettes(
    cassettes: &BTreeMap<&str, &HidCassette>,
    ledger: &LedgerIndex<'_>,
    observed: &mut BTreeMap<CountKey, u32>,
) -> Result<(), FixtureError> {
    for (case, cassette) in cassettes {
        let mut extractor = ProtocolIdentityExtractor::default();
        for exchange in &cassette.exchanges {
            let Some(response) = exchange.response.as_deref() else {
                return invalid(format!(
                    "cassette {case} contains unsupported response-less traffic"
                ));
            };
            let inspection = extractor
                .inspect_exchange(&exchange.request, response)
                .map_err(|error| {
                    FixtureError::invalid(
                        "fixture verification",
                        format!("cassette {case}: {error}"),
                    )
                })?;
            if inspection.request_match != exchange.request_match {
                return invalid(format!(
                    "cassette {case} uses the wrong request matching rule"
                ));
            }
            for field in inspection.fields {
                let value = field.value(response).map_err(|error| {
                    FixtureError::invalid("fixture verification", error.to_string())
                })?;
                if value.iter().all(|byte| *byte == 0) {
                    continue;
                }
                let principal = ledger.resolve_bytes(field.kind, value)?;
                observe(
                    observed,
                    principal,
                    field.kind,
                    IdentityLocation::Cassette {
                        case: (*case).to_string(),
                        channel: cassette.channel.clone(),
                    },
                );
            }
        }
    }
    Ok(())
}

fn resolve_device_route<'a>(
    ledger: &'a LedgerIndex<'_>,
    route: &FixtureDeviceRoute,
) -> Result<&'a str, FixtureError> {
    let mut matches = ledger.principals.iter().filter_map(|(id, principal)| {
        matches!(principal, FixturePrincipal::Device { route: candidate, .. } if candidate == route)
            .then_some(*id)
    });
    let Some(principal) = matches.next() else {
        return invalid(format!("profile device route {route:?} has no principal"));
    };
    if matches.next().is_some() {
        return invalid(format!("profile device route {route:?} is ambiguous"));
    }
    Ok(principal)
}

fn require_device_route(
    ledger: &LedgerIndex<'_>,
    principal: &str,
    route: &FixtureDeviceRoute,
) -> Result<(), FixtureError> {
    if ledger.device_route(principal) == Some(route) {
        Ok(())
    } else {
        invalid(format!(
            "identity principal {principal} occurs at wrong profile route"
        ))
    }
}

fn observe_bytes_for_principal(
    observed: &mut BTreeMap<CountKey, u32>,
    ledger: &LedgerIndex<'_>,
    principal: &str,
    kind: SyntheticIdentityKind,
    value: &[u8],
    field: ProfileIdentityField,
) -> Result<(), FixtureError> {
    let actual = ledger.resolve_bytes(kind, value)?;
    if actual != principal {
        return invalid(format!(
            "{kind:?} belongs to principal {actual}, not profile principal {principal}"
        ));
    }
    observe_profile(observed, principal, kind, field);
    Ok(())
}

fn observe_profile_for_principal(
    observed: &mut BTreeMap<CountKey, u32>,
    ledger: &LedgerIndex<'_>,
    principal: &str,
    kind: SyntheticIdentityKind,
    value: &str,
    field: ProfileIdentityField,
) -> Result<(), FixtureError> {
    let actual = ledger.resolve_profile(kind, value)?;
    if actual != principal {
        return invalid(format!(
            "{kind:?} belongs to principal {actual}, not profile principal {principal}"
        ));
    }
    observe_profile(observed, principal, kind, field);
    Ok(())
}

fn observe_profile(
    observed: &mut BTreeMap<CountKey, u32>,
    principal: &str,
    kind: SyntheticIdentityKind,
    field: ProfileIdentityField,
) {
    observe(
        observed,
        principal,
        kind,
        IdentityLocation::Profile { field },
    );
}

fn observe(
    observed: &mut BTreeMap<CountKey, u32>,
    principal: &str,
    kind: SyntheticIdentityKind,
    location: IdentityLocation,
) {
    let count = observed
        .entry(CountKey {
            principal: principal.to_string(),
            kind,
            location,
        })
        .or_default();
    *count = count.saturating_add(1);
}

fn compare_counts(
    expected: &BTreeMap<CountKey, u32>,
    observed: &BTreeMap<CountKey, u32>,
) -> Result<(), FixtureError> {
    if expected == observed {
        return Ok(());
    }
    if let Some((key, expected_count)) = expected
        .iter()
        .find(|(key, count)| observed.get(*key) != Some(*count))
    {
        let actual = observed.get(key).copied().unwrap_or(0);
        return invalid(format!(
            "identity count mismatch for principal {} {:?} at {:?}: expected {}, found {actual}",
            key.principal, key.kind, key.location, expected_count
        ));
    }
    let (key, actual) = observed
        .iter()
        .find(|(key, _)| !expected.contains_key(*key))
        .ok_or_else(|| FixtureError::invalid("fixture verification", "identity count mismatch"))?;
    invalid(format!(
        "extra identity occurrence for principal {} {:?} at {:?}: found {actual}",
        key.principal, key.kind, key.location
    ))
}

fn invalid<T>(message: impl Into<String>) -> Result<T, FixtureError> {
    Err(FixtureError::invalid("fixture verification", message))
}
