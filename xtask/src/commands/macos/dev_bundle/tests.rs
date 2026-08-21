use super::*;

#[test]
fn the_profile_comes_from_the_binarys_own_directory() {
    let debug = Profile::of(Path::new("/repo/target/debug/openlogi-desktop")).unwrap();
    assert!(!debug.release);
    assert_eq!(debug.dir, Path::new("/repo/target/debug"));

    let release = Profile::of(Path::new("/repo/target/release/openlogi-desktop")).unwrap();
    assert!(release.release);
    assert_eq!(release.dir, Path::new("/repo/target/release"));
}

/// `CARGO_TARGET_DIR`, a shared target directory, or a git worktree all put
/// the build somewhere other than `<root>/target`, and the helpers have to
/// be looked for where Cargo actually wrote them.
#[test]
fn the_helper_directory_follows_cargo_rather_than_the_repo() {
    let profile =
        Profile::of(Path::new("/elsewhere/shared-target/debug/openlogi-desktop")).unwrap();

    assert_eq!(profile.dir, Path::new("/elsewhere/shared-target/debug"));
}

/// A cross-compiled or custom-profile layout is not something to guess at:
/// guessing would build the helpers from the wrong profile.
#[test]
fn an_unrecognised_profile_directory_is_an_error() {
    let error = Profile::of(Path::new("/repo/target/aarch64-apple-darwin/debug-fast/x"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("named debug or release"), "got: {error}");
}

/// The app seals its helpers, so its signature must be applied last.
#[test]
fn the_app_is_signed_after_everything_it_contains() {
    let app = Path::new("/repo/target/dev/OpenLogi.app");
    let order = sign_order(app, Component::VARIANTS);
    assert_eq!(order.last().unwrap(), app);
    assert_eq!(
        order.len(),
        Component::VARIANTS.len(),
        "every component must be signed exactly once"
    );
}

#[test]
fn skipping_the_helpers_leaves_only_the_app_to_sign() {
    let app = Path::new("/repo/target/dev/OpenLogi.app");
    assert_eq!(sign_order(app, &APP_ONLY), vec![app.to_path_buf()]);
}
