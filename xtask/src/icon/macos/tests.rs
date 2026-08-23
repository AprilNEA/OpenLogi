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

/// The catalog [`IconPipeline::verify`] looks for first.
fn with_catalog(app: &Path) {
    fs_err::write(app.join("Contents/Resources").join(CATALOG), []).unwrap();
}

/// The preview every icon Settings offers has to ship.
fn with_previews(app: &Path) {
    for icon in AppIcon::ALL {
        let path = preview(app, icon);
        fs_err::create_dir_all(path.parent().unwrap()).unwrap();
        fs_err::write(&path, []).unwrap();
    }
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

/// Both halves of "a picker that points at nothing": the render it draws, and
/// the icon it would apply.
#[test]
fn a_bundle_missing_an_icon_preview_is_rejected() {
    let app = bundle();
    with_catalog(app.path());

    let error = AppBundle.verify(app.path()).unwrap_err().to_string();

    assert!(error.contains("icon preview"), "got: {error}");
}

#[test]
fn a_bundle_missing_an_alternate_icon_is_rejected() {
    let app = bundle();
    with_catalog(app.path());
    with_previews(app.path());

    let error = AppBundle.verify(app.path()).unwrap_err().to_string();

    assert!(error.contains("missing the prism icon"), "got: {error}");
}

#[test]
fn a_bundle_that_does_not_name_the_catalog_entry_is_rejected() {
    let app = bundle();
    with_catalog(app.path());
    with_previews(app.path());
    for icon in AppIcon::ALL {
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

    assert_eq!(alternate(app.path(), AppIcon::default()), None);
    assert!(
        AppIcon::ALL
            .iter()
            .any(|&icon| alternate(app.path(), icon).is_some()),
        "a set with no alternate would make the whole pipeline pointless"
    );
}
