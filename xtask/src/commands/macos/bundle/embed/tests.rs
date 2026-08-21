use strum::VariantArray as _;

use super::super::identity;
use super::super::info_plist::read_plist_string;
use super::*;
use crate::support::fs::repo_root;

/// Identity work iterates every `Component`, so a component added without a
/// `Helper` to embed it would only surface as a stamping failure during a
/// real build.
#[test]
fn every_nested_component_is_embedded_by_a_helper() {
    for &component in Component::VARIANTS {
        assert!(
            component == Component::App
                || HELPERS.iter().any(|helper| helper.component == component),
            "{component} has no Helper entry to embed it"
        );
    }
}

fn touch_all(binaries: &[std::path::PathBuf]) {
    for path in binaries {
        fs_err::create_dir_all(path.parent().unwrap()).unwrap();
        fs_err::write(path, b"").unwrap();
    }
}

/// Both channels lay their helpers out differently, and both are assembled
/// by this code — `macos bundle --channel dev` and `macos dev-bundle`.
#[test]
fn verify_bundle_binaries_accepts_a_complete_bundle() {
    for channel in [Channel::Production, Channel::Dev] {
        let app = tempfile::tempdir().unwrap();
        touch_all(&required_bundle_binaries(app.path(), channel));

        verify_bundle_binaries(app.path(), channel).unwrap();
    }
}

/// The checked-in helper plists are what a fresh bundle starts from, so a
/// rename there that never reached the identity table would ship one name in
/// the bundle and another in every verification.
#[test]
fn shipped_helper_plists_declare_their_production_identity() {
    let root = repo_root().unwrap();

    for helper in &HELPERS {
        let plist = root.join(helper.info_plist);
        let expected = Channel::Production.identity(helper.component);

        for (key, want) in identity::identity_entries(&expected) {
            assert_eq!(
                read_plist_string(&plist, key).unwrap().as_deref(),
                Some(want),
                "{} declares the wrong {key}",
                helper.info_plist
            );
        }
    }
}

/// Every helper must declare the shared icon, or it shows up blank in the
/// System Settings panes where users grant it permissions.
#[test]
fn shipped_helper_plists_declare_the_shared_icon() {
    let root = repo_root().unwrap();

    for helper in &HELPERS {
        let icon = read_plist_string(&root.join(helper.info_plist), "CFBundleIconFile").unwrap();

        assert_eq!(
            icon.as_deref().map(|file| file.trim_end_matches(".icns")),
            Some("AppIcon"),
            "{} must declare the shared app icon",
            helper.info_plist
        );
    }
}

#[test]
fn verify_bundle_binaries_names_each_missing_binary() {
    let channel = Channel::Production;
    let count = required_bundle_binaries(Path::new("/probe"), channel).len();

    for skipped in 0..count {
        let app = tempfile::tempdir().unwrap();
        let required = required_bundle_binaries(app.path(), channel);
        let missing = required[skipped].clone();
        let shipped: Vec<_> = required
            .into_iter()
            .filter(|path| *path != missing)
            .collect();
        touch_all(&shipped);

        let error = verify_bundle_binaries(app.path(), channel).unwrap_err();

        assert!(
            error.to_string().ends_with(&missing.display().to_string()),
            "error should name {}, got: {error}",
            missing.display()
        );
    }
}
