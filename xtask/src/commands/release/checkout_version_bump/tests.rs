use super::{Resolution, resolve};
use xshell::{Shell, cmd};

#[test]
fn existing_release_tag_skips_publishing() {
    let (_directory, sh) = repository("0.7.5");
    cmd!(sh, "git tag v0.7.5").run().unwrap();

    assert_eq!(resolve(&sh, "0.7.5").unwrap(), Resolution::AlreadyTagged);
}

#[test]
fn release_commit_subject_wins_over_manifest_history() {
    let (directory, sh) = repository("0.7.4");
    write_manifest(directory.path(), "0.7.5", "");
    commit(&sh, "manual version bump");
    fs_err::write(directory.path().join("release-marker"), "release").unwrap();
    let release_sha = commit(&sh, "chore: release v0.7.5 (#750)");

    assert_eq!(
        resolve(&sh, "0.7.5").unwrap(),
        Resolution::Commit(release_sha)
    );
}

#[test]
fn manifest_history_finds_a_nonstandard_version_bump() {
    let (directory, sh) = repository("0.7.4");
    write_manifest(directory.path(), "0.7.5", "");
    let bump_sha = commit(&sh, "manual version bump");
    write_manifest(directory.path(), "0.7.5", "# unrelated manifest edit\n");
    commit(&sh, "chore: edit manifest metadata");

    assert_eq!(resolve(&sh, "0.7.5").unwrap(), Resolution::Commit(bump_sha));
}

#[test]
fn a_missing_version_bump_is_an_error() {
    let (_directory, sh) = repository("0.7.4");

    let error = resolve(&sh, "0.7.5").unwrap_err();

    assert_eq!(
        error.to_string(),
        "could not locate the version-bump commit for v0.7.5"
    );
}

fn repository(version: &str) -> (tempfile::TempDir, Shell) {
    let directory = tempfile::tempdir().unwrap();
    let sh = Shell::new().unwrap();
    sh.change_dir(directory.path());
    cmd!(sh, "git init --quiet").run().unwrap();
    cmd!(sh, "git config user.name test").run().unwrap();
    cmd!(sh, "git config user.email test@example.com")
        .run()
        .unwrap();
    // The temp repo inherits the developer's global signing config, and a
    // locked signing agent then fails every test commit with exit 128 —
    // these fixtures must not depend on that state.
    cmd!(sh, "git config commit.gpgsign false").run().unwrap();
    write_manifest(directory.path(), version, "");
    commit(&sh, "initial version");
    (directory, sh)
}

fn write_manifest(root: &std::path::Path, version: &str, suffix: &str) {
    fs_err::write(
        root.join("Cargo.toml"),
        format!(
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"{version}\"\nrust-version = \"1.98\"\n{suffix}"
        ),
    )
    .unwrap();
}

fn commit(sh: &Shell, message: &str) -> String {
    cmd!(sh, "git add --all").run().unwrap();
    cmd!(sh, "git commit --quiet --message {message}")
        .run()
        .unwrap();
    cmd!(sh, "git rev-parse HEAD").read().unwrap()
}
