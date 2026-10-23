use std::path::Path;
use std::process::Command;

use semver::Version;

use super::{Member, Stamped, stamp};

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
        error.contains("the requirement on core is \"0.8\""),
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
