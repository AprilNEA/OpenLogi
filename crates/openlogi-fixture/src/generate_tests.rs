use super::{
    CANONICAL_DEVICE_PROFILE_JSON, CassetteExchange, DeviceProfile, FIXTURE_SCHEMA_VERSION,
    FixtureCaseBinding, FixtureManifest, FixtureVerificationStage, HidCassette, ReportSupport,
    RequestMatch,
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

#[test]
fn generation_and_verification_require_actual_case_request_targets() {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    // Slot 1, slot 3, and direct traffic are distinct even without identities.
    for (setting, correct, wrong) in [(2, 3, 1), (0, 1, 3), (2, 3, 0xff), (3, 0xff, 1)] {
        let mut cassette = bolt_identity_cassette(b"OL-BOLT-UID-0001");
        cassette.exchanges = vec![CassetteExchange {
            request_match: RequestMatch::Hidpp20,
            request: vec![0x10, correct, 0, 0x10, 0, 0, 0],
            response: Some(vec![0x10, correct, 0, 0x10, 4, 0, 0]),
            required: true,
        }];
        let binding = FixtureCaseBinding {
            name: cassette.name.clone(),
            route: profile.settings[setting].route.clone(),
        };
        let manifest = FixtureManifest::from_assets(
            "target-relationship".to_string(),
            &profile,
            std::slice::from_ref(&cassette),
            std::slice::from_ref(&binding),
        )
        .expect("the declared device's ping is valid");
        manifest
            .verify(&profile, std::slice::from_ref(&cassette))
            .expect("matching request target verifies");

        cassette.exchanges[0].request[1] = wrong;
        cassette.exchanges[0].response.as_mut().unwrap()[1] = wrong;
        let error = manifest
            .verify_detailed(&profile, std::slice::from_ref(&cassette))
            .expect_err("an identity-free ping cannot cross device routes");
        assert_eq!(error.stage(), FixtureVerificationStage::Relationship);
        FixtureManifest::from_assets(
            "wrong-target".to_string(),
            &profile,
            &[cassette],
            &[binding],
        )
        .expect_err("generation must reject the same target mismatch");
    }
}

#[test]
fn device_cases_allow_receiver_setup_only_for_receiver_routes() {
    let profile: DeviceProfile =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
    let mut cassette = bolt_identity_cassette(b"OL-BOLT-UID-0001");
    cassette.exchanges.insert(
        0,
        CassetteExchange {
            request_match: RequestMatch::Exact,
            request: vec![0x10, 0xff, 0x81, 0x02, 0, 0, 0],
            response: Some(vec![0x10, 0xff, 0x81, 0x02, 0, 3, 0]),
            required: true,
        },
    );
    FixtureManifest::from_assets(
        "receiver-setup".to_string(),
        &profile,
        std::slice::from_ref(&cassette),
        &[FixtureCaseBinding {
            name: cassette.name.clone(),
            route: profile.settings[2].route.clone(),
        }],
    )
    .expect("a slot-3 case may read receiver setup and receiver identity");

    cassette.exchanges.truncate(1);
    for setting in [3, 4] {
        FixtureManifest::from_assets(
            "invalid-receiver-setup".to_string(),
            &profile,
            std::slice::from_ref(&cassette),
            &[FixtureCaseBinding {
                name: cassette.name.clone(),
                route: profile.settings[setting].route.clone(),
            }],
        )
        .expect_err("direct and raw-HID devices have no receiver setup traffic");
    }
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
