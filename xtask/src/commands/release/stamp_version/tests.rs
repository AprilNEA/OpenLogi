use std::path::Path;
use std::process::Command;

use semver::Version;

use xshell::Shell;

use super::{Member, Stamped, restore, stamp, stamp_checked};

const ROOT: &str = r#"[workspace]
members = ["app", "core"]
resolver = "3"

[workspace.package]
version = "0.8.17"
edition = "2024"

[workspace.dependencies]
shared = { package = "core", path = "core", version = "0.8.17" } # the shared crate
serde = "1"
"#;

const APP: &str = r#"[package]
name = "app"
version.workspace = true
edition.workspace = true

[dependencies]
shared = { package = "core", path = "../core", version = "0.8.17" }

[dev-dependencies]
shared = { workspace = true }

[target.'cfg(unix)'.build-dependencies.shared]
package = "core"
path = "../core"
version = "0.8.17"
"#;

const CORE: &str = r#"[package]
name = "core"
version.workspace = true
edition.workspace = true
"#;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs_err::create_dir_all(parent).unwrap();
    }
    fs_err::write(path, contents).unwrap();
}

fn read(path: &Path) -> String {
    fs_err::read_to_string(path).unwrap()
}

fn cargo(root: &Path, args: &[&str]) -> std::process::Output {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    Command::new(cargo)
        .args(args)
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

/// A two-crate workspace at `0.8.17` with its lock file, and its members.
fn workspace(root: &Path, app: &str) -> Vec<Member> {
    write(&root.join("Cargo.toml"), ROOT);
    write(&root.join("app/Cargo.toml"), app);
    write(&root.join("app/src/lib.rs"), "");
    write(&root.join("core/Cargo.toml"), CORE);
    write(&root.join("core/src/lib.rs"), "");
    let output = cargo(root, &["generate-lockfile", "--offline"]);
    assert!(output.status.success(), "{output:?}");
    ["app", "core"]
        .into_iter()
        .map(|name| Member {
            name: name.into(),
            manifest: root.join(name).join("Cargo.toml"),
        })
        .collect()
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

#[test]
fn a_stamp_moves_the_workspace_its_requirements_and_its_lock_entries() {
    let root = tempfile::tempdir().unwrap();
    let members = workspace(root.path(), APP);

    let stamped = stamp(root.path(), &members, &version("0.8.18-beta.3")).unwrap();

    assert_eq!(
        stamped,
        Stamped::Changed {
            from: version("0.8.17"),
            requirements: 3,
            locked: 2,
        }
    );
    let manifest = read(&root.path().join("Cargo.toml"));
    assert_eq!(
        manifest,
        ROOT.replace("0.8.17", "0.8.18-beta.3"),
        "only the versions change, comments and all"
    );
    assert!(manifest.contains("serde = \"1\""), "{manifest}");
    assert_eq!(
        read(&root.path().join("app/Cargo.toml")),
        APP.replace("0.8.17", "0.8.18-beta.3")
    );
    assert_eq!(read(&root.path().join("core/Cargo.toml")), CORE);

    // A `--locked` build resolves the stamped tree as it is.
    let output = cargo(
        root.path(),
        &["metadata", "--locked", "--offline", "--format-version", "1"],
    );
    assert!(output.status.success(), "{output:?}");
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for package in metadata["packages"].as_array().unwrap() {
        assert_eq!(package["version"], "0.8.18-beta.3", "{}", package["name"]);
    }
}

#[test]
fn a_workspace_already_at_the_version_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let members = workspace(root.path(), APP);
    let lock = read(&root.path().join("Cargo.lock"));

    let stamped = stamp(root.path(), &members, &version("0.8.17")).unwrap();

    assert_eq!(stamped, Stamped::Unchanged);
    assert_eq!(read(&root.path().join("Cargo.toml")), ROOT);
    assert_eq!(read(&root.path().join("Cargo.lock")), lock);
}

#[test]
fn a_requirement_release_plz_would_not_write_stops_the_stamp() {
    let root = tempfile::tempdir().unwrap();
    let app = APP.replace(
        r#"shared = { package = "core", path = "../core", version = "0.8.17" }"#,
        r#"shared = { package = "core", path = "../core", version = "0.8" }"#,
    );
    let members = workspace(root.path(), &app);
    let lock = read(&root.path().join("Cargo.lock"));

    let error = stamp(root.path(), &members, &version("0.8.18-beta.3")).unwrap_err();

    let error = format!("{error:#}");
    assert!(
        error.contains("the requirement on core (shared) is \"0.8\""),
        "{error}"
    );
    assert!(error.contains("[dependencies]"), "{error}");
    assert_eq!(
        read(&root.path().join("Cargo.toml")),
        ROOT,
        "nothing is written when any edit fails"
    );
    assert_eq!(read(&root.path().join("app/Cargo.toml")), app);
    assert_eq!(read(&root.path().join("Cargo.lock")), lock);
}

#[test]
fn a_lock_file_missing_a_workspace_crate_stops_the_stamp() {
    let root = tempfile::tempdir().unwrap();
    let mut members = workspace(root.path(), APP);
    members.push(Member {
        name: "absent".into(),
        manifest: root.path().join("core/Cargo.toml"),
    });

    let error = stamp(root.path(), &members, &version("0.8.18-beta.3")).unwrap_err();

    assert!(
        format!("{error:#}").contains("lists 2 of the 3 workspace crates"),
        "{error:#}"
    );
    assert_eq!(read(&root.path().join("Cargo.toml")), ROOT);
}

/// A registry crate can share a workspace crate's name and even its version.
/// It is a dependency by name only, so neither its requirement nor its lock
/// entry moves.
#[test]
fn a_registry_crate_with_a_workspace_crates_name_keeps_its_version() {
    let root = tempfile::tempdir().unwrap();
    let members = workspace(root.path(), APP);
    // Added after the lock file, which offline cargo could not resolve.
    let registry = r#"upstream = { package = "core", version = "0.8.17" }"#;
    let app_path = root.path().join("app/Cargo.toml");
    let app = read(&app_path).replace(
        "[dev-dependencies]",
        &format!("{registry}\n\n[dev-dependencies]"),
    );
    write(&app_path, &app);
    let lock_path = root.path().join("Cargo.lock");
    let source = r#"source = "registry+https://github.com/rust-lang/crates.io-index""#;
    let lock = format!(
        "{}\n[[package]]\nname = \"core\"\nversion = \"0.8.17\"\n{source}\n",
        read(&lock_path).replace(
            "\"core\",\n]",
            "\"core 0.8.17\",\n \"core 0.8.17 (registry+https://github.com/rust-lang/crates.io-index)\",\n]"
        )
    );
    write(&lock_path, &lock);

    let stamped = stamp(root.path(), &members, &version("0.8.18-beta.3")).unwrap();

    assert_eq!(
        stamped,
        Stamped::Changed {
            from: version("0.8.17"),
            requirements: 3,
            locked: 2,
        }
    );
    let app = read(&app_path);
    assert!(app.contains(registry), "{app}");
    let lock = read(&lock_path);
    assert!(
        lock.contains(&format!("name = \"core\"\nversion = \"0.8.17\"\n{source}")),
        "{lock}"
    );
    assert!(
        lock.contains("\"core 0.8.17 (registry+https://github.com/rust-lang/crates.io-index)\""),
        "{lock}"
    );
    assert!(lock.contains("\"core 0.8.18-beta.3\""), "{lock}");
}

/// A `[patch]` that points at a workspace crate names its version, which
/// must follow; a patch from elsewhere keeps its own.
#[test]
fn a_patch_on_a_workspace_crate_moves_with_it() {
    let root = tempfile::tempdir().unwrap();
    let members = workspace(root.path(), APP);
    let patches = r#"
[patch.crates-io]
core = { path = "core", version = "0.8.17" }
elsewhere = { git = "https://example.com/elsewhere", version = "0.8.17" }
"#;
    let manifest_path = root.path().join("Cargo.toml");
    write(&manifest_path, &format!("{ROOT}{patches}"));

    let stamped = stamp(root.path(), &members, &version("0.8.18-beta.3")).unwrap();

    assert_eq!(
        stamped,
        Stamped::Changed {
            from: version("0.8.17"),
            requirements: 4,
            locked: 2,
        }
    );
    let manifest = read(&manifest_path);
    assert!(
        manifest.contains(r#"core = { path = "core", version = "0.8.18-beta.3" }"#),
        "{manifest}"
    );
    assert!(
        manifest.contains(
            r#"elsewhere = { git = "https://example.com/elsewhere", version = "0.8.17" }"#
        ),
        "{manifest}"
    );
}

fn offline_shell() -> Shell {
    let sh = Shell::new().unwrap();
    sh.set_var("CARGO_NET_OFFLINE", "true");
    sh
}

#[test]
fn a_checked_stamp_resolves_as_a_locked_build_would() {
    let root = tempfile::tempdir().unwrap();
    let members = workspace(root.path(), APP);

    let stamped = stamp_checked(
        &offline_shell(),
        root.path(),
        &members,
        &version("0.8.18-beta.3"),
    )
    .unwrap();

    assert!(matches!(stamped, Stamped::Changed { .. }), "{stamped:?}");
    assert!(read(&root.path().join("Cargo.lock")).contains("0.8.18-beta.3"));
}

/// The check runs after every file is written. When the tree does not
/// resolve, each file goes back to its bytes from before.
#[test]
fn a_stamp_that_does_not_resolve_puts_every_file_back() {
    let root = tempfile::tempdir().unwrap();
    let members = workspace(root.path(), APP);
    // A path dependency the lock file has never seen: `--locked` refuses to
    // add it, so the stamped tree fails the check.
    write(
        &root.path().join("helper/Cargo.toml"),
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    write(&root.path().join("helper/src/lib.rs"), "");
    let app_path = root.path().join("app/Cargo.toml");
    let app = read(&app_path).replace(
        "[dev-dependencies]",
        "helper = { path = \"../helper\" }\n\n[dev-dependencies]",
    );
    write(&app_path, &app);
    let files = [
        "Cargo.toml",
        "Cargo.lock",
        "app/Cargo.toml",
        "core/Cargo.toml",
    ];
    let before: Vec<String> = files
        .iter()
        .map(|file| read(&root.path().join(file)))
        .collect();

    let error = stamp_checked(
        &offline_shell(),
        root.path(),
        &members,
        &version("0.8.18-beta.3"),
    )
    .unwrap_err();

    let error = format!("{error:#}");
    assert!(
        error.contains("does not resolve with `--locked`"),
        "{error}"
    );
    assert!(error.contains("the tree is as it was"), "{error}");
    for (file, before) in files.iter().zip(&before) {
        assert_eq!(&read(&root.path().join(file)), before, "{file}");
    }
}

/// A root that is also a package is one manifest, whatever its path's
/// spelling. Both the workspace version and the root's own dependency on a
/// member move, and neither write undoes the other.
#[test]
fn a_root_manifest_spelled_two_ways_gets_both_edits() {
    let root = tempfile::tempdir().unwrap();
    let manifest = r#"[workspace]
members = ["core"]
resolver = "3"

[workspace.package]
version = "0.8.17"
edition = "2024"

[package]
name = "root"
version.workspace = true
edition.workspace = true

[dependencies]
core = { path = "core", version = "0.8.17" }
"#;
    write(&root.path().join("Cargo.toml"), manifest);
    write(&root.path().join("src/lib.rs"), "");
    write(&root.path().join("core/Cargo.toml"), CORE);
    write(&root.path().join("core/src/lib.rs"), "");
    let output = cargo(root.path(), &["generate-lockfile", "--offline"]);
    assert!(output.status.success(), "{output:?}");
    // The same file as `<root>/Cargo.toml`, by another path.
    let members = vec![
        Member {
            name: "root".into(),
            manifest: root.path().join("core/../Cargo.toml"),
        },
        Member {
            name: "core".into(),
            manifest: root.path().join("core/Cargo.toml"),
        },
    ];

    let stamped = stamp_checked(
        &offline_shell(),
        root.path(),
        &members,
        &version("0.8.18-beta.3"),
    )
    .unwrap();

    assert_eq!(
        stamped,
        Stamped::Changed {
            from: version("0.8.17"),
            requirements: 1,
            locked: 2,
        }
    );
    assert_eq!(
        read(&root.path().join("Cargo.toml")),
        manifest.replace("0.8.17", "0.8.18-beta.3")
    );
}

/// Restoring goes on past a file it cannot write, and names that file.
#[test]
fn a_file_that_cannot_be_restored_does_not_stop_the_others() {
    let root = tempfile::tempdir().unwrap();
    // Its directory is gone, so writing it back fails.
    let lost = root.path().join("gone/Cargo.toml");
    let kept = root.path().join("kept/Cargo.toml");
    write(&kept, "stamped");

    let failed = restore(&[
        (b"before".to_vec(), lost.clone()),
        (b"before".to_vec(), kept.clone()),
    ]);

    assert_eq!(read(&kept), "before");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].contains(&lost.display().to_string()),
        "{failed:?}"
    );
}
