use std::collections::BTreeSet;

use super::*;
use crate::support::fs::repo_root;

/// Two shipped locales sharing one `.lproj` would let one overwrite the
/// other's strings without an error.
#[test]
fn every_shipped_locale_has_its_own_apple_localization() {
    let names: BTreeSet<&str> = SUPPORTED
        .iter()
        .map(|&(code, _)| apple_localization(code))
        .collect();
    assert_eq!(names.len(), SUPPORTED.len());
}

/// `localize_app` reads the real catalogs at bundle time, so a renamed or
/// removed key has to fail here rather than in a release build.
#[test]
fn shipped_catalogs_carry_every_localized_key() {
    let locales = repo_root().unwrap().join(LOCALES_DIR);
    for &(code, _) in SUPPORTED {
        let catalog = read_catalog(&locales, code).unwrap();
        localized_values(&catalog, code).unwrap();
    }
}

#[test]
fn a_catalog_missing_a_localized_key_names_the_file_and_key() {
    let catalog: Table = toml::from_str("[permissions]\n").unwrap();
    let error = localized_values(&catalog, "fr").unwrap_err().to_string();
    assert!(
        error.contains("fr.toml") && error.contains("permissions.camera_usage_description"),
        "{error}"
    );
}

/// Writes one catalog per shipped locale into `locales`, with `value` for
/// every locale except Spanish.
fn write_catalogs(locales: &Path, value: &str, spanish: &str) {
    fs_err::create_dir_all(locales).unwrap();
    for &(code, _) in SUPPORTED {
        let text = if code == "es" { spanish } else { value };
        fs_err::write(
            locales.join(format!("{code}.toml")),
            format!("[permissions]\ncamera_usage_description = \"{text}\"\n"),
        )
        .unwrap();
    }
}

fn empty_app(root: &Path) -> std::path::PathBuf {
    let app = root.join("OpenLogi.app");
    fs_err::create_dir_all(app.join("Contents/Resources")).unwrap();
    Value::Dictionary(plist::Dictionary::new())
        .to_file_xml(app.join("Contents/Info.plist"))
        .unwrap();
    app
}

#[test]
fn localize_app_writes_strings_only_for_translated_locales() {
    let dir = tempfile::tempdir().unwrap();
    let app = empty_app(dir.path());
    let locales = dir.path().join("locales");
    write_catalogs(&locales, "Local preview", "Vista previa local");

    localize_app(&app, &locales).unwrap();

    let info_plist = app.join("Contents/Info.plist");
    assert_eq!(
        read_plist_string(&info_plist, "NSCameraUsageDescription")
            .unwrap()
            .as_deref(),
        Some("Local preview")
    );
    let declared: Vec<String> = Value::from_file(&info_plist)
        .unwrap()
        .as_dictionary()
        .unwrap()
        .get("CFBundleLocalizations")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|localization| localization.as_string().unwrap().to_owned())
        .collect();
    let expected: Vec<String> = SUPPORTED
        .iter()
        .map(|&(code, _)| apple_localization(code).to_owned())
        .collect();
    assert_eq!(declared, expected);

    let resources = app.join("Contents/Resources");
    let spanish = resources.join("es.lproj/InfoPlist.strings");
    assert_eq!(
        read_plist_string(&spanish, "NSCameraUsageDescription")
            .unwrap()
            .as_deref(),
        Some("Vista previa local")
    );
    let written: Vec<_> = fs_err::read_dir(&resources)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(written, ["es.lproj"]);
}

/// The dev bundle is rebuilt in place: a translation that has since been
/// reverted to the English fill-in must not survive from an earlier run, and
/// an `.lproj` holding something else must keep it.
#[test]
fn localize_app_drops_strings_a_previous_run_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let app = empty_app(dir.path());
    let resources = app.join("Contents/Resources");
    let locales = dir.path().join("locales");
    write_catalogs(&locales, "Local preview", "Vista previa local");
    localize_app(&app, &locales).unwrap();
    fs_err::create_dir_all(resources.join("fr.lproj")).unwrap();
    fs_err::write(resources.join("fr.lproj/InfoPlist.strings"), "stale").unwrap();
    fs_err::write(resources.join("fr.lproj/Other.strings"), "kept").unwrap();

    write_catalogs(&locales, "Local preview", "Local preview");
    localize_app(&app, &locales).unwrap();

    assert!(!resources.join("es.lproj").exists());
    assert!(!resources.join("fr.lproj/InfoPlist.strings").exists());
    assert!(resources.join("fr.lproj/Other.strings").exists());
}

#[test]
fn localize_app_refuses_an_empty_english_value() {
    let dir = tempfile::tempdir().unwrap();
    let app = empty_app(dir.path());
    let locales = dir.path().join("locales");
    write_catalogs(&locales, " ", "Vista previa local");

    let error = localize_app(&app, &locales).unwrap_err().to_string();
    assert!(error.contains("NSCameraUsageDescription"), "{error}");
}
