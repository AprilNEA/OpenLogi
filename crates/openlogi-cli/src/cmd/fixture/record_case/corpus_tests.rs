//! Reviewed corpus contracts, independent of the production decoder output.

use super::{FixtureOperation, SemanticObservation, WriteError};
use openlogi_device::replay::ReplayBackend;
use openlogi_fixture::{ProfileSetting, fs::repository_corpus};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Diagnostics {
    cases: Vec<String>,
    // Feature ID, version, type bits; control ID, task ID, capability bits.
    features: Vec<(u16, u8, u8)>,
    controls: Vec<(u16, u16, u16)>,
    raw_battery: String,
}

#[tokio::test]
async fn contributed_corpus_replays_every_declared_operation() {
    let Some(corpus) = repository_corpus().expect("valid repository corpus") else {
        return;
    };
    let mut expectations: BTreeMap<String, Diagnostics> =
        serde_json::from_str(include_str!("corpus_diagnostics.json"))
            .expect("reviewed diagnostics");
    for fixture in corpus {
        let specimen = &fixture.manifest().id;
        let expected = expectations
            .remove(specimen)
            .unwrap_or_else(|| panic!("{specimen}: missing reviewed operation expectations"));
        assert_eq!(
            fixture
                .cassettes()
                .iter()
                .map(|case| &case.name)
                .collect::<Vec<_>>(),
            expected.cases.iter().collect::<Vec<_>>(),
            "{specimen}: recorded case coverage changed"
        );
        for cassette in fixture.cassettes() {
            let case = &cassette.name;
            let operation = FixtureOperation::ALL
                .into_iter()
                .find(|operation| operation.slug() == case)
                .unwrap_or_else(|| panic!("{specimen}/{case}: no production operation"));
            let (backend, route) = ReplayBackend::from_profile(fixture.profile(), cassette)
                .expect("single-device contribution");
            let settings = &fixture.profile().settings[0];
            let observed = operation.observe(&backend, &route).await;
            backend
                .require_complete()
                .unwrap_or_else(|error| panic!("{specimen}/{case}: {error}"));
            match observed {
                SemanticObservation::FeatureTable(result) => assert_eq!(
                    result
                        .expect("feature table")
                        .iter()
                        .map(|entry| (entry.id, entry.version, entry.typ.bits()))
                        .collect::<Vec<_>>(),
                    expected.features,
                    "{specimen}/{case}"
                ),
                SemanticObservation::ReprogrammableControls(result) => assert_eq!(
                    result
                        .expect("control table")
                        .iter()
                        .map(|entry| (entry.cid, entry.task_id, entry.flags.bits()))
                        .collect::<Vec<_>>(),
                    expected.controls,
                    "{specimen}/{case}"
                ),
                SemanticObservation::RawBattery(result) => assert_eq!(
                    result.expect("battery diagnostic"),
                    expected.raw_battery,
                    "{specimen}/{case}"
                ),
                SemanticObservation::DpiInfo(result) => {
                    assert_setting(&settings.dpi, result, specimen, case);
                }
                SemanticObservation::SmartshiftStatus(result) => {
                    assert_setting(&settings.smartshift, result, specimen, case);
                }
                SemanticObservation::WheelMode(result) => {
                    assert_setting(&settings.wheel, result, specimen, case);
                }
                SemanticObservation::BacklightState(result) => {
                    assert_setting(&settings.backlight, result, specimen, case);
                }
                SemanticObservation::FirmwareEntities(_) => {
                    panic!("{specimen}/{case}: firmware needs independently reviewed expectations")
                }
            }
        }
    }
    assert!(
        expectations.is_empty(),
        "reviewed specimens disappeared: {:?}",
        expectations.keys()
    );
}

fn assert_setting<T: std::fmt::Debug + PartialEq>(
    expected: &ProfileSetting<T>,
    actual: Result<T, WriteError>,
    specimen: &str,
    case: &str,
) {
    match expected {
        ProfileSetting::Supported(value) => {
            assert_eq!(
                actual.expect("supported recorded setting"),
                *value,
                "{specimen}/{case}"
            );
        }
        ProfileSetting::Unsupported => assert!(
            matches!(actual, Err(WriteError::FeatureUnsupported { .. })),
            "{specimen}/{case}: {actual:?}"
        ),
        ProfileSetting::Unavailable => panic!(
            "{specimen}/{case}: unavailable profile value needs a reviewed error expectation"
        ),
    }
}
