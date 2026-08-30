use crate::DeviceRoute;

use super::*;

#[test]
fn canonical_manifest_round_trips_and_verifies_the_profile_without_claiming_cases() {
    let manifest = canonical_manifest();
    let profile = canonical_profile();
    manifest.validate().expect("canonical manifest validates");
    manifest
        .verify(&profile, &[])
        .expect("canonical profile matches its exact identity ledger");
    assert!(manifest.cases.is_empty());

    let encoded = serde_json::to_value(&manifest).expect("manifest serializes");
    let reparsed: FixtureManifest =
        serde_json::from_value(encoded).expect("serialized manifest reparses");
    assert_eq!(reparsed, manifest);
}

#[test]
fn manifest_rejects_unknown_fields_recursively() {
    let canonical: serde_json::Value =
        serde_json::from_str(CANONICAL_FIXTURE_MANIFEST_JSON).expect("canonical manifest is JSON");
    let reject = |mut manifest: serde_json::Value, pointer: &str| {
        manifest
            .pointer_mut(pointer)
            .expect("test pointer exists")
            .as_object_mut()
            .expect("test pointer names an object")
            .insert("unexpected".to_string(), serde_json::Value::Bool(true));
        let error = serde_json::from_value::<FixtureManifest>(manifest)
            .expect_err("unknown nested manifest field must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");
    };

    reject(canonical.clone(), "");
    reject(canonical.clone(), "/identity_ledger/0/principal");
    reject(canonical.clone(), "/identity_ledger/1/principal/route");
    reject(canonical.clone(), "/identity_ledger/0/representations/0");
    reject(
        canonical.clone(),
        "/identity_ledger/0/representations/0/occurrences/0",
    );
    reject(
        canonical,
        "/identity_ledger/0/representations/0/occurrences/0/location",
    );

    let mut with_case = serde_json::to_value(case_fixture().0).expect("case manifest serializes");
    with_case["cases"][0]["relationship"]
        .as_object_mut()
        .expect("relationship is an object")
        .insert("unexpected".to_string(), serde_json::Value::Bool(true));
    let error = serde_json::from_value::<FixtureManifest>(with_case)
        .expect_err("unknown case relationship field must be rejected");
    assert!(error.to_string().contains("unknown field"), "{error}");
}

#[test]
fn verifier_rejects_real_values_wrong_principals_and_profile_route_mismatches() {
    let manifest = canonical_manifest();

    let mut real = canonical_profile();
    real.inventories[0].receiver.unique_id = Some("ABCDEF0123456789".to_string());
    for settings in &mut real.settings {
        if let DeviceRoute::Bolt { receiver_uid, .. } = &mut settings.route {
            *receiver_uid = "ABCDEF0123456789".to_string();
        }
    }
    let error = manifest
        .verify(&real, &[])
        .expect_err("real-looking receiver identity must be rejected");
    assert!(error.to_string().contains("not a canonical synthetic"));

    let mut swapped = canonical_profile();
    let mouse = swapped.inventories[0].paired[0]
        .model_info
        .as_mut()
        .expect("canonical mouse has model info")
        .unit_id;
    let keyboard = swapped.inventories[0].paired[2]
        .model_info
        .as_mut()
        .expect("canonical keyboard has model info")
        .unit_id;
    swapped.inventories[0].paired[0]
        .model_info
        .as_mut()
        .expect("canonical mouse has model info")
        .unit_id = keyboard;
    swapped.inventories[0].paired[2]
        .model_info
        .as_mut()
        .expect("canonical keyboard has model info")
        .unit_id = mouse;
    let error = manifest
        .verify(&swapped, &[])
        .expect_err("synthetic value on the wrong principal must be rejected");
    assert!(error.to_string().contains("not profile principal"));

    let mut wrong_route = canonical_manifest();
    let FixturePrincipal::Device { route, .. } = &mut wrong_route.identity_ledger[1].principal
    else {
        panic!("canonical second principal is a device");
    };
    *route = FixtureDeviceRoute::Bolt {
        receiver: "bolt-receiver".to_string(),
        slot: 4,
    };
    let error = wrong_route
        .verify(&canonical_profile(), &[])
        .expect_err("manifest route mismatch must be rejected");
    assert!(error.to_string().contains("has no principal"));
}

#[test]
fn verifier_rejects_missing_and_extra_occurrence_counts() {
    let mut missing = canonical_profile();
    missing.inventories[0].paired[0]
        .model_info
        .as_mut()
        .expect("canonical mouse has model info")
        .unit_id = [0; 4];
    let error = canonical_manifest()
        .verify(&missing, &[])
        .expect_err("missing profile occurrence must fail");
    assert!(error.to_string().contains("expected 1, found 0"));

    let (manifest, profile, cassette) = case_fixture();
    manifest
        .verify(&profile, std::slice::from_ref(&cassette))
        .expect("single declared cassette occurrence verifies");
    let mut extra = cassette;
    extra.exchanges.push(extra.exchanges[0].clone());
    let error = manifest
        .verify(&profile, &[extra])
        .expect_err("repeated cassette identity must exceed the exact count");
    assert!(error.to_string().contains("expected 1, found 2"));

    let mut wrong_expected = canonical_manifest();
    let IdentityRepresentation::DeviceUnitId { occurrences, .. } =
        &mut wrong_expected.identity_ledger[1].representations[0]
    else {
        panic!("canonical mouse uses a unit ID");
    };
    occurrences[0].count = 2;
    let error = wrong_expected
        .verify(&canonical_profile(), &[])
        .expect_err("ledger count larger than evidence must fail");
    assert!(error.to_string().contains("expected 2, found 1"));
}

#[test]
fn manifest_rejects_duplicate_values_and_wrong_unifying_hex_relation() {
    let mut duplicate = canonical_manifest();
    let IdentityRepresentation::DeviceUnitId { value: mouse, .. } =
        duplicate.identity_ledger[1].representations[0]
    else {
        panic!("canonical mouse uses a unit ID");
    };
    let IdentityRepresentation::DeviceUnitId {
        value: keyboard, ..
    } = &mut duplicate.identity_ledger[2].representations[0]
    else {
        panic!("canonical keyboard uses a unit ID");
    };
    *keyboard = mouse;
    let error = duplicate
        .validate()
        .expect_err("one synthetic value cannot identify two principals");
    assert!(error.to_string().contains("assigned more than once"));

    let mut unifying = unifying_manifest();
    unifying
        .validate()
        .expect("exact binary-to-uppercase-hex relation validates");
    let IdentityRepresentation::UnifyingReceiverSerial { profile_route, .. } =
        &mut unifying.identity_ledger[0].representations[0]
    else {
        panic!("test manifest has a Unifying identity");
    };
    *profile_route = "4F4C5202".to_string();
    let error = unifying
        .validate()
        .expect_err("wrong Unifying profile route must fail");
    assert!(error.to_string().contains("profile route disagree"));
}

#[test]
fn verifier_rejects_case_channel_relationship_and_unsupported_identity_traffic() {
    let (manifest, profile, cassette) = case_fixture();

    let mut wrong_channel = cassette.clone();
    wrong_channel.channel = "other-channel".to_string();
    let error = manifest
        .verify(&profile, &[wrong_channel])
        .expect_err("cassette channel must match its named case");
    assert!(error.to_string().contains("uses channel"));

    let mut wrong_relationship = manifest.clone();
    wrong_relationship.cases[0].relationship = FixtureCaseRelationship::Device {
        device: "direct-mouse".to_string(),
    };
    let error = wrong_relationship
        .validate()
        .expect_err("case occurrence must relate to its principal");
    assert!(error.to_string().contains("unrelated"));

    let mut unsupported = cassette;
    unsupported.exchanges = vec![
        h20(short(1, 0, 0, [0, 7, 0]), short(1, 0, 0, [8, 0, 0])),
        h20(short(1, 8, 0, [0; 3]), short(1, 8, 0, [1, 2, 3])),
    ];
    let error = manifest
        .verify(&profile, &[unsupported])
        .expect_err("unsupported identity-bearing feature must fail closed");
    assert!(error.to_string().contains("unsupported identity-bearing"));
}

fn canonical_manifest() -> FixtureManifest {
    serde_json::from_str(CANONICAL_FIXTURE_MANIFEST_JSON).expect("canonical manifest parses")
}

fn canonical_profile() -> DeviceProfile {
    serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses")
}

fn case_fixture() -> (FixtureManifest, DeviceProfile, HidCassette) {
    let mut manifest = canonical_manifest();
    manifest.cases.push(FixtureCase {
        name: "bolt-identity".to_string(),
        channel: "bolt-receiver".to_string(),
        relationship: FixtureCaseRelationship::Receiver {
            receiver: "bolt-receiver".to_string(),
        },
    });
    let IdentityRepresentation::BoltReceiverUid { occurrences, .. } =
        &mut manifest.identity_ledger[0].representations[0]
    else {
        panic!("canonical receiver uses a Bolt UID");
    };
    occurrences.push(IdentityOccurrence {
        location: IdentityLocation::Cassette {
            case: "bolt-identity".to_string(),
            channel: "bolt-receiver".to_string(),
        },
        count: 1,
    });
    let cassette = HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "bolt-identity".to_string(),
        channel: "bolt-receiver".to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges: vec![CassetteExchange {
            request_match: RequestMatch::Exact,
            request: short(0xff, 0x83, 0xfb, [0; 3]),
            response: Some(long(0xff, 0x83, 0xfb, b"OL-BOLT-UID-0001")),
            required: true,
        }],
    };
    (manifest, canonical_profile(), cassette)
}

fn unifying_manifest() -> FixtureManifest {
    FixtureManifest {
        schema_version: FIXTURE_SCHEMA_VERSION,
        id: "unifying-fixture".to_string(),
        profile_id: "unifying-profile".to_string(),
        cases: Vec::new(),
        identity_ledger: vec![IdentityLedgerEntry {
            principal: FixturePrincipal::Receiver {
                id: "receiver".to_string(),
            },
            representations: vec![IdentityRepresentation::UnifyingReceiverSerial {
                value: [b'O', b'L', b'R', 1],
                profile_route: "4F4C5201".to_string(),
                binary_occurrences: Vec::new(),
                route_occurrences: vec![IdentityOccurrence {
                    location: IdentityLocation::Profile {
                        field: ProfileIdentityField::ReceiverIdentity,
                    },
                    count: 1,
                }],
            }],
        }],
    }
}

fn h20(request: Vec<u8>, response: Vec<u8>) -> CassetteExchange {
    CassetteExchange {
        request_match: RequestMatch::Hidpp20,
        request,
        response: Some(response),
        required: true,
    }
}

fn short(device: u8, feature: u8, function: u8, payload: [u8; 3]) -> Vec<u8> {
    vec![
        0x10, device, feature, function, payload[0], payload[1], payload[2],
    ]
}

fn long(device: u8, feature: u8, function: u8, payload: &[u8]) -> Vec<u8> {
    let mut report = vec![0x11, device, feature, function];
    report.extend_from_slice(payload);
    report.resize(20, 0);
    report
}
