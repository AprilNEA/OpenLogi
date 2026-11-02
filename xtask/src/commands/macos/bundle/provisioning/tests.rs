use std::path::Path;
use std::time::{Duration, SystemTime};

use plist::{Date, Dictionary, Value};

use super::{AgentProfile, EMBEDDED_PROFILE, HID_VIRTUAL_DEVICE};

const AGENT: &str = "org.openlogi.agent";
const TEAM: &str = "ABCDE12345";
const CERT: &[u8] = b"leaf certificate";

/// The day the tests run "at": every profile below expires a year later.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
}

/// A Developer ID profile for the agent, as `security cms -D` decodes one.
fn developer_id_profile() -> Dictionary {
    let mut entitlements = Dictionary::new();
    entitlements.insert(
        "com.apple.application-identifier".into(),
        Value::String(format!("{TEAM}.{AGENT}")),
    );
    entitlements.insert("com.apple.developer.team-identifier".into(), TEAM.into());
    entitlements.insert(HID_VIRTUAL_DEVICE.into(), Value::Boolean(true));
    entitlements.insert(
        "keychain-access-groups".into(),
        Value::Array(vec![format!("{TEAM}.*").into()]),
    );
    let mut profile = Dictionary::new();
    profile.insert("Platform".into(), Value::Array(vec!["OSX".into()]));
    profile.insert("ProvisionsAllDevices".into(), Value::Boolean(true));
    profile.insert("TeamIdentifier".into(), Value::Array(vec![TEAM.into()]));
    profile.insert("Entitlements".into(), Value::Dictionary(entitlements));
    profile.insert(
        "ExpirationDate".into(),
        Value::Date(Date::from(now() + Duration::from_hours(365 * 24))),
    );
    profile.insert(
        "DeveloperCertificates".into(),
        Value::Array(vec![Value::Data(CERT.to_vec())]),
    );
    profile
}

fn entitlements_mut(profile: &mut Dictionary) -> &mut Dictionary {
    profile
        .get_mut("Entitlements")
        .and_then(Value::as_dictionary_mut)
        .unwrap()
}

fn check(profile: Dictionary) -> anyhow::Result<AgentProfile> {
    AgentProfile::from_plist(
        Path::new("agent.provisionprofile"),
        &Value::Dictionary(profile),
        AGENT,
        now(),
    )
}

fn rejection(profile: Dictionary) -> String {
    format!("{:#}", check(profile).unwrap_err())
}

#[test]
fn accepts_a_developer_id_profile_for_the_agent() {
    let profile = check(developer_id_profile()).unwrap();
    assert_eq!(profile.app_id, format!("{TEAM}.{AGENT}"));
    assert_eq!(profile.team, TEAM);
    assert_eq!(profile.certificates, vec![CERT.to_vec()]);
}

#[test]
fn claims_only_what_the_virtual_keyboard_needs() {
    let entitlements = check(developer_id_profile()).unwrap().entitlements();
    let mut keys: Vec<&str> = entitlements.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "com.apple.application-identifier",
            "com.apple.developer.hid.virtual.device",
            "com.apple.developer.team-identifier",
        ],
        "keychain-access-groups is granted but not needed, so it is not claimed"
    );
    assert_eq!(
        entitlements
            .get("com.apple.application-identifier")
            .and_then(Value::as_string),
        Some(format!("{TEAM}.{AGENT}").as_str())
    );
    assert_eq!(
        entitlements
            .get(HID_VIRTUAL_DEVICE)
            .and_then(Value::as_boolean),
        Some(true)
    );
}

#[test]
fn rejects_a_profile_for_another_app_id() {
    for app_id in [format!("{TEAM}.{AGENT}-dev"), format!("{TEAM}.*")] {
        let mut profile = developer_id_profile();
        entitlements_mut(&mut profile).insert(
            "com.apple.application-identifier".into(),
            Value::String(app_id.clone()),
        );
        assert!(
            rejection(profile).contains("but the agent is"),
            "{app_id} must not sign the agent"
        );
    }
}

#[test]
fn rejects_a_profile_without_the_hid_grant() {
    let mut profile = developer_id_profile();
    entitlements_mut(&mut profile).remove(HID_VIRTUAL_DEVICE);
    assert!(rejection(profile).contains("does not grant"));
}

#[test]
fn rejects_a_development_profile() {
    let mut profile = developer_id_profile();
    profile.remove("ProvisionsAllDevices");
    profile.insert(
        "ProvisionedDevices".into(),
        Value::Array(vec!["00006000-0000000000000000".into()]),
    );
    assert!(rejection(profile).contains("registered devices"));
}

#[test]
fn rejects_an_expired_profile() {
    let mut profile = developer_id_profile();
    profile.insert(
        "ExpirationDate".into(),
        Value::Date(Date::from(now() - Duration::from_secs(1))),
    );
    assert!(rejection(profile).contains("expired"));
}

#[test]
fn rejects_a_profile_for_another_platform() {
    let mut profile = developer_id_profile();
    profile.insert("Platform".into(), Value::Array(vec!["iOS".into()]));
    assert!(rejection(profile).contains("not a macOS profile"));
}

#[test]
fn rejects_a_team_mismatch() {
    let mut profile = developer_id_profile();
    entitlements_mut(&mut profile).insert(
        "com.apple.developer.team-identifier".into(),
        "ZZZZZ99999".into(),
    );
    assert!(rejection(profile).contains("does not match"));
}

#[test]
fn rejects_a_profile_without_certificates() {
    let mut profile = developer_id_profile();
    profile.insert("DeveloperCertificates".into(), Value::Array(Vec::new()));
    assert!(rejection(profile).contains("no signing certificate"));
}

#[test]
fn install_embeds_the_profile_and_writes_the_claim() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("agent.provisionprofile");
    fs_err::write(&source, b"signed profile bytes").unwrap();
    let agent = dir.path().join("OpenLogi Agent.app");
    fs_err::create_dir_all(agent.join("Contents")).unwrap();
    let scratch = tempfile::tempdir().unwrap();

    let profile = AgentProfile::from_plist(
        &source,
        &Value::Dictionary(developer_id_profile()),
        AGENT,
        now(),
    )
    .unwrap();
    let entitlements = profile.install(&agent, scratch.path()).unwrap();

    assert_eq!(
        fs_err::read(agent.join(EMBEDDED_PROFILE)).unwrap(),
        b"signed profile bytes"
    );
    let written = Value::from_file(&entitlements).unwrap();
    assert_eq!(written.as_dictionary(), Some(&profile.entitlements()));
}
