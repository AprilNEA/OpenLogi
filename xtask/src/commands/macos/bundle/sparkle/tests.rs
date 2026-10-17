use plist::{Dictionary, Value};

use super::{Feed, code_inside_out, configure, framework};
use crate::support::update_feed::MacArch;

/// The public half of the seed `[7; 32]`, as Sparkle 2.10's `generate_keys`
/// prints it (the golden key `release appcast`'s tests sign with).
const PUBLIC_KEY: &str = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=";

#[test]
fn feed_points_each_architecture_at_its_own_appcast() {
    let feed = Feed::new(PUBLIC_KEY, "https://updates.openlogi.org/", MacArch::X86_64).unwrap();
    assert_eq!(
        feed.url,
        "https://updates.openlogi.org/channels/stable/appcast-x86_64.xml"
    );
    // Surrounding whitespace from a pasted secret is not part of the key.
    let feed = Feed::new(
        &format!(" {PUBLIC_KEY}\n"),
        "https://u.example",
        MacArch::Arm64,
    )
    .unwrap();
    assert_eq!(feed.public_key, PUBLIC_KEY);
    assert_eq!(
        feed.url,
        "https://u.example/channels/stable/appcast-arm64.xml"
    );
}

#[test]
fn feed_rejects_keys_that_are_not_an_ed25519_public_key() {
    for key in ["not base64!", "AAAA", ""] {
        assert!(
            Feed::new(key, "https://u.example", MacArch::Arm64).is_err(),
            "{key:?}"
        );
    }
    // A legacy 96-byte private key export is not a public key either.
    let legacy = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [0_u8; 96]);
    Feed::new(&legacy, "https://u.example", MacArch::Arm64).unwrap_err();
}

#[test]
fn info_plist_keeps_sparkle_from_polling_and_verifies_before_extracting() {
    let feed = Feed::new(PUBLIC_KEY, "https://u.example", MacArch::Arm64).unwrap();
    let entries: Dictionary = feed
        .info_plist_entries()
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    let boolean = |key| entries.get(key).and_then(Value::as_boolean);
    assert_eq!(boolean("SUEnableAutomaticChecks"), Some(false));
    assert_eq!(boolean("SUAllowsAutomaticUpdates"), Some(true));
    assert_eq!(boolean("SUEnableSystemProfiling"), Some(false));
    assert_eq!(boolean("SUVerifyUpdateBeforeExtraction"), Some(true));
    assert_eq!(
        entries.get("SUPublicEDKey").and_then(Value::as_string),
        Some(PUBLIC_KEY)
    );
    assert_eq!(
        entries.get("SUFeedURL").and_then(Value::as_string),
        Some("https://u.example/channels/stable/appcast-arm64.xml")
    );
}

#[test]
fn configure_stamps_the_keys_beside_what_the_bundle_already_declares() {
    let dir = tempfile::tempdir().unwrap();
    let info_plist = dir.path().join("Info.plist");
    let mut existing = Dictionary::new();
    existing.insert("CFBundleIdentifier".into(), "org.openlogi.openlogi".into());
    existing.insert("CFBundleShortVersionString".into(), "0.8.14".into());
    existing.insert("CFBundleVersion".into(), "0.8.14".into());
    // A stale value from an earlier stamp is replaced, not kept.
    existing.insert("SUEnableAutomaticChecks".into(), Value::Boolean(true));
    Value::Dictionary(existing)
        .to_file_xml(&info_plist)
        .unwrap();

    let feed = Feed::new(PUBLIC_KEY, "https://u.example", MacArch::Arm64).unwrap();
    configure(&info_plist, &feed).unwrap();

    let plist = Value::from_file(&info_plist).unwrap();
    let dict = plist.as_dictionary().unwrap();
    assert_eq!(
        dict.get("CFBundleIdentifier").and_then(Value::as_string),
        Some("org.openlogi.openlogi")
    );
    assert_eq!(
        dict.get("SUEnableAutomaticChecks")
            .and_then(Value::as_boolean),
        Some(false)
    );
}

#[test]
fn configure_refuses_a_bundle_version_sparkle_would_rank_above_every_release() {
    let dir = tempfile::tempdir().unwrap();
    let info_plist = dir.path().join("Info.plist");
    let feed = Feed::new(PUBLIC_KEY, "https://u.example", MacArch::Arm64).unwrap();
    // cargo-bundle's own `CFBundleVersion` is the build's UTC timestamp; a
    // missing one is refused as well.
    for bundle_version in [Some("20261012.120227"), None] {
        let mut existing = Dictionary::new();
        existing.insert("CFBundleShortVersionString".into(), "0.8.14".into());
        if let Some(bundle_version) = bundle_version {
            existing.insert("CFBundleVersion".into(), bundle_version.into());
        }
        Value::Dictionary(existing)
            .to_file_xml(&info_plist)
            .unwrap();
        let error = configure(&info_plist, &feed).unwrap_err().to_string();
        assert!(error.contains("CFBundleVersion"), "{error}");
    }
}

#[test]
fn sparkle_code_is_signed_inside_out_and_only_when_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("OpenLogi.app");
    assert!(code_inside_out(&app).is_empty());

    let framework = framework(&app);
    fs_err::create_dir_all(&framework).unwrap();
    let order = code_inside_out(&app);
    let version = framework.join("Versions/B");
    assert_eq!(
        order,
        [
            version.join("Autoupdate"),
            version.join("Updater.app"),
            framework,
        ]
    );
}
