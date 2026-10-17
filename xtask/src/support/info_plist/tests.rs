use plist::{Dictionary, Value};

use super::stamp_launch_services_keys;

#[test]
fn launch_services_keys_match_what_cargo_bundle_generated() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("OpenLogi.app");
    let info_plist = app.join("Contents/Info.plist");
    fs_err::create_dir_all(info_plist.parent().unwrap()).unwrap();
    let mut existing = Dictionary::new();
    existing.insert("CFBundleIdentifier".into(), "org.openlogi.openlogi".into());
    // A stale URL type from an earlier stamp is replaced, not appended to.
    let mut stale = Dictionary::new();
    stale.insert(
        "CFBundleURLSchemes".into(),
        Value::Array(vec!["stale".into()]),
    );
    existing.insert(
        "CFBundleURLTypes".into(),
        Value::Array(vec![Value::Dictionary(stale)]),
    );
    Value::Dictionary(existing)
        .to_file_xml(&info_plist)
        .unwrap();

    stamp_launch_services_keys(&app).unwrap();

    let plist = Value::from_file(&info_plist).unwrap();
    let dict = plist.as_dictionary().unwrap();
    assert_eq!(
        dict.get("CFBundleIdentifier").and_then(Value::as_string),
        Some("org.openlogi.openlogi")
    );
    assert_eq!(
        dict.get("LSMinimumSystemVersion")
            .and_then(Value::as_string),
        Some("13.0")
    );
    let url_types = dict
        .get("CFBundleURLTypes")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(url_types.len(), 1);
    let url_type = url_types[0].as_dictionary().unwrap();
    assert_eq!(
        url_type.get("CFBundleURLName").and_then(Value::as_string),
        Some("OpenLogi")
    );
    assert_eq!(
        url_type.get("CFBundleTypeRole").and_then(Value::as_string),
        Some("Viewer")
    );
    assert_eq!(
        url_type.get("CFBundleURLSchemes"),
        Some(&Value::Array(vec!["openlogi".into()]))
    );
}
