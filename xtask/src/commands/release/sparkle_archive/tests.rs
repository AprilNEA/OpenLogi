use super::*;

/// A small app-shaped tree: an executable, a resource and a framework link.
fn app(root: &Path) -> PathBuf {
    let app = root.join("OpenLogi.app");
    let macos = app.join("Contents/MacOS");
    fs_err::create_dir_all(&macos).unwrap();
    fs_err::write(macos.join("openlogi-desktop"), b"\xcf\xfa\xed\xfe binary").unwrap();
    fs_err::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let binary = macos.join("openlogi-desktop");
        fs_err::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("Versions/Current/Sparkle", app.join("Contents/Sparkle"))
            .unwrap();
    }
    app
}

#[test]
fn the_archive_is_named_for_its_dmg() {
    assert_eq!(
        archive_name("OpenLogi-v0.8.21-macos-arm64.dmg").unwrap(),
        "OpenLogi-v0.8.21-macos-arm64.tar.xz"
    );
    archive_name("OpenLogi-v0.8.21-macos-arm64.zip").unwrap_err();
}

#[test]
fn the_archive_decodes_to_the_tar() {
    let tar = b"OpenLogi.app/Contents/Info.plist\0".repeat(64);

    let xz = compress(&tar).unwrap();

    assert_eq!(liblzma::decode_all(xz.as_slice()).unwrap(), tar);
}

#[test]
fn identical_trees_match() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    ensure_same_tree(&app(a.path()), &app(b.path())).unwrap();
}

#[test]
fn a_changed_byte_is_named() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let unpacked = app(b.path());
    fs_err::write(unpacked.join("Contents/Info.plist"), b"<plist />").unwrap();

    let error = ensure_same_tree(&app(a.path()), &unpacked).unwrap_err();

    assert!(error.to_string().contains("Contents/Info.plist"), "{error}");
}

#[test]
fn a_missing_or_added_file_fails() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (expected, unpacked) = (app(a.path()), app(b.path()));
    fs_err::write(unpacked.join("Contents/._Info.plist"), b"AppleDouble").unwrap();

    let added = ensure_same_tree(&expected, &unpacked).unwrap_err();
    let missing = ensure_same_tree(&unpacked, &expected).unwrap_err();

    assert!(added.to_string().contains("adds"), "{added}");
    assert!(missing.to_string().contains("missing"), "{missing}");
}

#[cfg(unix)]
#[test]
fn a_lost_executable_bit_or_link_target_fails() {
    use std::os::unix::fs::PermissionsExt as _;
    let (a, b, c) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let expected = app(a.path());
    let no_exec = app(b.path());
    fs_err::set_permissions(
        no_exec.join("Contents/MacOS/openlogi-desktop"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let relinked = app(c.path());
    fs_err::remove_file(relinked.join("Contents/Sparkle")).unwrap();
    std::os::unix::fs::symlink("Versions/B/Sparkle", relinked.join("Contents/Sparkle")).unwrap();

    for unpacked in [no_exec, relinked] {
        ensure_same_tree(&expected, &unpacked).unwrap_err();
    }
}
