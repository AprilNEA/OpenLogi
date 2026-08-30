use super::{
    CANONICAL_DEVICE_PROFILE_JSON, CassetteExchange, DeviceProfile, FIXTURE_SCHEMA_VERSION,
    FixtureCaseBinding, FixtureManifest, HidCassette, ReportSupport, RequestMatch,
};

#[test]
fn generates_exact_profile_only_manifest() {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");

    let manifest =
        FixtureManifest::from_assets("generated-profile-only".to_string(), &profile, &[], &[])
            .expect("profile-only manifest is generated");

    assert_eq!(manifest.profile_id, profile.id);
    assert!(manifest.cases.is_empty());
    assert!(!manifest.identity_ledger.is_empty());
    manifest
        .verify(&profile, &[])
        .expect("generated manifest verifies exact profile evidence");
}

#[test]
fn generates_case_relationships_and_exact_cassette_counts() {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    let route = profile.settings[0].route.clone();
    let cassette = bolt_identity_cassette(b"OL-BOLT-UID-0001");

    let manifest = FixtureManifest::from_assets(
        "generated-with-case".to_string(),
        &profile,
        std::slice::from_ref(&cassette),
        &[FixtureCaseBinding {
            name: cassette.name.clone(),
            route,
        }],
    )
    .expect("case manifest is generated");

    assert_eq!(manifest.cases.len(), 1);
    manifest
        .verify(&profile, &[cassette])
        .expect("generated case manifest verifies");
}

#[test]
fn generation_rejects_cassette_identities_absent_from_the_profile() {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    let cassette = bolt_identity_cassette(b"OL-BOLT-UID-0002");

    FixtureManifest::from_assets(
        "generated-with-mismatch".to_string(),
        &profile,
        std::slice::from_ref(&cassette),
        &[FixtureCaseBinding {
            name: cassette.name.clone(),
            route: profile.settings[0].route.clone(),
        }],
    )
    .expect_err("an independently numbered cassette must fail closed");
}

fn bolt_identity_cassette(identity: &[u8; 16]) -> HidCassette {
    let mut response = vec![0x11, 0xff, 0x83, 0xfb];
    response.extend_from_slice(identity);
    HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "bolt-identity".to_string(),
        channel: "target".to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges: vec![CassetteExchange {
            request_match: RequestMatch::Exact,
            request: vec![0x10, 0xff, 0x83, 0xfb, 0, 0, 0],
            response: Some(response),
            required: true,
        }],
    }
}
