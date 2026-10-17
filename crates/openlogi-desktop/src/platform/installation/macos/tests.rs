use std::fs;
use std::os::unix::fs::symlink;

use super::*;

fn bundle(root: &Path) -> PathBuf {
    fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
    fs::write(root.join("Contents/Info.plist"), "<plist/>").unwrap();
    let executable = root.join("Contents/MacOS/openlogi-desktop");
    fs::write(&executable, "binary").unwrap();
    executable
}

fn receipt(prefix: &Path, token: &str, target: &Path) -> PathBuf {
    let caskroom = prefix.join("Caskroom").join(token);
    let metadata = caskroom.join(".metadata/1.0/20260101/Casks");
    fs::create_dir_all(&metadata).unwrap();
    fs::write(metadata.join(format!("{token}.json")), "{}").unwrap();
    fs::write(
        caskroom.join(".metadata/INSTALL_RECEIPT.json"),
        r#"{"source":{"version":"1.0"},"uninstall_artifacts":[{"app":["OpenLogi.app"]}]}"#,
    )
    .unwrap();
    fs::create_dir_all(caskroom.join("1.0")).unwrap();
    symlink(target, caskroom.join("1.0/OpenLogi.app")).unwrap();
    caskroom
}

#[test]
fn both_casks_match_custom_appdirs_without_comparing_app_versions() {
    for (token, expected) in [
        ("openlogi", HomebrewCask::Official),
        ("openlogi@latest", HomebrewCask::Latest),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("My Apps/OpenLogi.app");
        let executable = bundle(&app);
        // The receipt remains 1.0 after an in-app update to 2.0.
        fs::write(
            app.join("Contents/Info.plist"),
            "<plist><string>2.0</string></plist>",
        )
        .unwrap();
        let prefix = temp.path().join("custom-brew");
        receipt(&prefix, token, &app);
        assert_eq!(
            detect_in(&executable, &[prefix.clone(), prefix]),
            InstallationSource::Homebrew(expected)
        );
    }
}

#[test]
fn another_copy_or_stale_backlink_does_not_own_this_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let installed = temp.path().join("Applications/OpenLogi.app");
    bundle(&installed);
    let running = bundle(&temp.path().join("Downloads/OpenLogi.app"));
    let prefix = temp.path().join("brew");
    receipt(&prefix, "openlogi", &installed);
    assert_eq!(
        detect_in(&running, std::slice::from_ref(&prefix)),
        InstallationSource::MacAppBundle
    );
    fs::remove_dir_all(&installed).unwrap();
    assert_eq!(
        detect_in(&running, &[prefix]),
        InstallationSource::MacAppBundle
    );
}

#[test]
fn requires_receipt_installed_caskfile_and_a_backlink_not_a_staged_directory() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("OpenLogi.app");
    let executable = bundle(&app);
    let prefix = temp.path().join("brew");
    let caskroom = receipt(&prefix, "openlogi", &app);
    let backlink = caskroom.join("1.0/OpenLogi.app");
    fs::remove_file(&backlink).unwrap();
    fs::create_dir(&backlink).unwrap();
    assert_eq!(
        detect_in(&executable, std::slice::from_ref(&prefix)),
        InstallationSource::MacAppBundle
    );
    fs::remove_dir(&backlink).unwrap();
    symlink(&app, &backlink).unwrap();

    let installed = caskroom.join(".metadata/1.0/20260101/Casks/openlogi.json");
    fs::remove_file(&installed).unwrap();
    assert_eq!(
        detect_in(&executable, std::slice::from_ref(&prefix)),
        InstallationSource::MacAppBundle
    );
    fs::write(&installed, "{}").unwrap();
    for invalid in [
        "broken",
        r#"{"source":{"version":"../1.0"}}"#,
        r#"{"source":{"version":""}}"#,
    ] {
        fs::write(caskroom.join(".metadata/INSTALL_RECEIPT.json"), invalid).unwrap();
        assert_eq!(
            detect_in(&executable, std::slice::from_ref(&prefix)),
            InstallationSource::MacAppBundle
        );
    }
    fs::remove_file(caskroom.join(".metadata/INSTALL_RECEIPT.json")).unwrap();
    assert_eq!(
        detect_in(&executable, &[prefix]),
        InstallationSource::MacAppBundle
    );
}

#[test]
fn conflicting_casks_are_unknown_instead_of_picking_the_first() {
    let temp = tempfile::tempdir().unwrap();
    let app = temp.path().join("OpenLogi.app");
    let executable = bundle(&app);
    let prefix = temp.path().join("brew");
    receipt(&prefix, "openlogi", &app);
    receipt(&prefix, "openlogi@latest", &app);
    assert_eq!(
        detect_in(&executable, &[prefix]),
        InstallationSource::Unknown
    );
}

#[test]
fn a_bare_binary_or_app_named_directory_is_not_a_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("OpenLogi.app/openlogi-desktop");
    fs::create_dir(executable.parent().unwrap()).unwrap();
    fs::write(&executable, "binary").unwrap();
    assert_eq!(detect(&executable), InstallationSource::Unknown);
    let executable = bundle(&temp.path().join("Real.app"));
    fs::remove_file(temp.path().join("Real.app/Contents/Info.plist")).unwrap();
    assert_eq!(detect_in(&executable, &[]), InstallationSource::Unknown);
}
