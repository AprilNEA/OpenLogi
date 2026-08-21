use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;

/// The `[workspace.package]` fields every crate inherits with
/// `field.workspace = true`.
///
/// Read from disk rather than baked in with `env!("CARGO_PKG_VERSION")`: the
/// release-plz workflow builds this binary on `master` and runs it against a
/// checked-out release branch, where the version is the bumped one.
#[derive(Deserialize)]
pub(crate) struct WorkspacePackage {
    /// The single version the whole workspace shares.
    pub(crate) version: String,
    /// The MSRV floor, which the `msrv` CI job pins `RUSTUP_TOOLCHAIN` to.
    #[serde(rename = "rust-version")]
    pub(crate) rust_version: String,
}

#[derive(Deserialize)]
struct Manifest {
    workspace: Workspace,
}

#[derive(Deserialize)]
struct Workspace {
    package: WorkspacePackage,
}

/// Read `[workspace.package]` out of the root `Cargo.toml` under `root`.
pub(crate) fn workspace_package(root: &Path) -> Result<WorkspacePackage> {
    let path = root.join("Cargo.toml");
    let text = fs_err::read_to_string(&path)?;
    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(manifest.workspace.package)
}
