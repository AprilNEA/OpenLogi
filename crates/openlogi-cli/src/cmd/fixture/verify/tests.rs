use std::path::{Path, PathBuf};

use openlogi_device::fixture::{
    CANONICAL_DEVICE_PROFILE_JSON, CANONICAL_FIXTURE_MANIFEST_JSON, CassetteExchange,
    FIXTURE_SCHEMA_VERSION, FixtureCase, FixtureCaseRelationship, FixtureManifest, HidCassette,
    IdentityLocation, IdentityOccurrence, IdentityRepresentation, ReportSupport, RequestMatch,
};

use super::*;

#[test]
fn repository_fixture_corpus_is_complete_and_valid() {
    let corpus =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../openlogi-device/tests/fixtures/devices");
    require_directory(&corpus, "repository fixture corpus").expect("corpus root is real");
    let entries = read_directory(&corpus, "repository fixture corpus").expect("read corpus root");
    assert!(!entries.is_empty(), "repository fixture corpus is empty");
    for entry in entries {
        require_entry_directory(&entry, "repository fixture directory")
            .expect("corpus contains only real fixture directories");
        LoadedFixture::load(&entry.path())
            .and_then(|fixture| fixture.verify())
            .unwrap_or_else(|error| panic!("{}: {error:#}", entry.path().display()));
    }
}

#[test]
fn canonical_profile_only_fixture_loads_without_fabricated_cases() {
    let fixture = fixture_directory();
    let loaded = LoadedFixture::load(&fixture).expect("canonical fixture loads");
    loaded.verify().expect("canonical fixture verifies");
    assert!(loaded.manifest.cases.is_empty());
    assert!(loaded.cassettes.is_empty());
}

#[test]
fn rejects_missing_extra_and_undeclared_case_files() {
    let (temp, fixture) = copied_canonical_fixture();
    std::fs::remove_file(fixture.join(PROFILE_FILE)).expect("remove profile");
    let error = LoadedFixture::load(&fixture).expect_err("missing profile must fail");
    assert!(error.to_string().contains("no profile.json"), "{error:#}");

    std::fs::write(fixture.join(PROFILE_FILE), CANONICAL_DEVICE_PROFILE_JSON)
        .expect("restore profile");
    std::fs::write(fixture.join("notes.txt"), "not part of the corpus").expect("seed extra file");
    let error = LoadedFixture::load(&fixture).expect_err("extra root file must fail");
    assert!(error.to_string().contains("unexpected entry"), "{error:#}");

    std::fs::remove_file(fixture.join("notes.txt")).expect("remove extra file");
    std::fs::create_dir(fixture.join(CASES_DIRECTORY)).expect("create undeclared cases directory");
    let error = LoadedFixture::load(&fixture).expect_err("undeclared cases directory must fail");
    assert!(
        error.to_string().contains("undeclared cases directory"),
        "{error:#}"
    );
    drop(temp);
}

#[test]
fn loads_exact_declared_case_set_and_rejects_missing_or_extra_files() {
    let (_temp, fixture) = copied_canonical_fixture();
    let (manifest, cassette) = manifest_with_case();
    std::fs::write(
        fixture.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest).expect("serialize case manifest"),
    )
    .expect("write case manifest");
    let cases = fixture.join(CASES_DIRECTORY);
    std::fs::create_dir(&cases).expect("create cases directory");
    std::fs::write(
        cases.join("bolt-identity.json"),
        serde_json::to_vec_pretty(&cassette).expect("serialize cassette"),
    )
    .expect("write cassette");

    let loaded = LoadedFixture::load(&fixture).expect("declared case set loads");
    loaded.verify().expect("declared case set verifies");
    assert_eq!(loaded.cassettes, vec![cassette]);

    std::fs::write(cases.join("extra.json"), b"{}").expect("write extra case");
    let error = LoadedFixture::load(&fixture).expect_err("extra case file must fail");
    assert!(
        error.to_string().contains("undeclared fixture case file"),
        "{error:#}"
    );
    std::fs::remove_file(cases.join("extra.json")).expect("remove extra case");
    std::fs::remove_file(cases.join("bolt-identity.json")).expect("remove declared case");
    let error = LoadedFixture::load(&fixture).expect_err("missing case file must fail");
    assert!(
        error
            .to_string()
            .contains("missing declared fixture case file"),
        "{error:#}"
    );
}

#[test]
fn rejects_case_path_traversal_before_reading_any_case() {
    let (_temp, fixture) = copied_canonical_fixture();
    let mut manifest: FixtureManifest =
        serde_json::from_str(CANONICAL_FIXTURE_MANIFEST_JSON).expect("canonical manifest");
    manifest.cases.push(FixtureCase {
        name: "../outside".to_string(),
        channel: "direct".to_string(),
        relationship: FixtureCaseRelationship::Device {
            device: "direct-mouse".to_string(),
        },
    });
    std::fs::write(
        fixture.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write traversal manifest");

    let error = LoadedFixture::load(&fixture).expect_err("case traversal must fail closed");
    assert!(error.to_string().contains("safe file name"), "{error:#}");
}

#[test]
fn rejects_unknown_fields_as_schema_failures() {
    let (_temp, fixture) = copied_canonical_fixture();
    let mut profile: serde_json::Value =
        serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile JSON");
    profile.as_object_mut().expect("profile object").insert(
        "host_path".to_string(),
        serde_json::json!("/private/device"),
    );
    std::fs::write(
        fixture.join(PROFILE_FILE),
        serde_json::to_vec_pretty(&profile).expect("serialize profile"),
    )
    .expect("write unknown field");

    let error = LoadedFixture::load(&fixture).expect_err("unknown profile field must fail");
    assert!(
        format!("{error:#}").contains("schema verification failed"),
        "{error:#}"
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_fixture_assets_and_directories() {
    use std::os::unix::fs::symlink;

    let (_temp, fixture) = copied_canonical_fixture();
    let profile = fixture.join(PROFILE_FILE);
    let real_profile = fixture.join("real-profile.json");
    std::fs::rename(&profile, &real_profile).expect("move profile");
    symlink(&real_profile, &profile).expect("symlink profile");
    let error = LoadedFixture::load(&fixture).expect_err("symlinked profile must fail");
    assert!(error.to_string().contains("non-symlink"), "{error:#}");

    let root_link = fixture
        .parent()
        .expect("fixture parent")
        .join("fixture-link");
    symlink(&fixture, &root_link).expect("symlink fixture directory");
    let error = LoadedFixture::load(&root_link).expect_err("symlinked root must fail");
    assert!(error.to_string().contains("non-symlink"), "{error:#}");
}

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../openlogi-device/tests/fixtures/devices/openlogi-canonical-synthetic-001")
}

fn copied_canonical_fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = temp.path().join("openlogi-canonical-synthetic-001");
    std::fs::create_dir(&fixture).expect("create fixture directory");
    std::fs::write(fixture.join(MANIFEST_FILE), CANONICAL_FIXTURE_MANIFEST_JSON)
        .expect("write manifest");
    std::fs::write(fixture.join(PROFILE_FILE), CANONICAL_DEVICE_PROFILE_JSON)
        .expect("write profile");
    (temp, fixture)
}

fn manifest_with_case() -> (FixtureManifest, HidCassette) {
    let mut manifest: FixtureManifest =
        serde_json::from_str(CANONICAL_FIXTURE_MANIFEST_JSON).expect("canonical manifest");
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

    let mut response = vec![0x11, 0xff, 0x83, 0xfb];
    response.extend_from_slice(b"OL-BOLT-UID-0001");
    let cassette = HidCassette {
        schema_version: FIXTURE_SCHEMA_VERSION,
        name: "bolt-identity".to_string(),
        channel: "bolt-receiver".to_string(),
        report_support: ReportSupport::ShortAndLong,
        exchanges: vec![CassetteExchange {
            request_match: RequestMatch::Exact,
            request: vec![0x10, 0xff, 0x83, 0xfb, 0, 0, 0],
            response: Some(response),
            required: true,
        }],
    };
    (manifest, cassette)
}
