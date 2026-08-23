use super::*;

/// A bundle carrying only an empty `Info.plist`, which is what
/// [`IconPipeline::install`] is handed after `cargo-bundle` runs.
fn bundle() -> tempfile::TempDir {
    let app = tempfile::tempdir().unwrap();
    fs_err::create_dir_all(app.path().join("Contents/Resources")).unwrap();
    plist::Value::Dictionary(plist::Dictionary::new())
        .to_file_xml(app.path().join("Contents/Info.plist"))
        .unwrap();
    app
}

/// Everything [`IconPipeline::verify`] wants except the icons themselves.
fn with_catalog(app: &Path) {
    fs_err::write(app.join("Contents/Resources").join(CATALOG), []).unwrap();
}

#[test]
fn a_bundle_without_the_asset_catalog_is_rejected() {
    let app = bundle();

    let error = AppBundle.verify(app.path()).unwrap_err().to_string();

    assert!(
        error.contains("missing the icon asset catalog"),
        "got: {error}"
    );
}

/// A bundle whose Settings would offer an icon it does not carry is a picker
/// pointing at nothing.
#[test]
fn a_bundle_missing_an_alternate_icon_is_rejected() {
    let app = bundle();
    with_catalog(app.path());

    let error = AppBundle.verify(app.path()).unwrap_err().to_string();

    assert!(error.contains("missing the midnight icon"), "got: {error}");
}

#[test]
fn a_bundle_that_does_not_name_the_catalog_entry_is_rejected() {
    let app = bundle();
    with_catalog(app.path());
    for &icon in AppIcon::VARIANTS {
        if let Some(path) = alternate(app.path(), icon) {
            fs_err::create_dir_all(path.parent().unwrap()).unwrap();
            fs_err::write(&path, []).unwrap();
        }
    }

    let error = AppBundle.verify(app.path()).unwrap_err().to_string();

    assert!(error.contains("CFBundleIconName"), "got: {error}");
}

/// The default icon *is* the bundle's own, so it never ships a second copy
/// under `Icons/`: the app clears the override to go back to it.
#[test]
fn only_the_alternates_ship_a_second_copy() {
    let app = bundle();

    assert_eq!(alternate(app.path(), AppIcon::DEFAULT), None);
    assert!(
        AppIcon::VARIANTS
            .iter()
            .any(|&icon| alternate(app.path(), icon).is_some()),
        "a set with no alternate would make the whole pipeline pointless"
    );
}
