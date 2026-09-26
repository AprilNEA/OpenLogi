use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use xshell::{Shell, cmd};

use super::arch::HostArch;
use crate::support::fs::{absolutize, ensure_command, ensure_file, repo_root};

#[derive(Parser)]
pub(crate) struct Args {
    /// Output directory for .deb, .rpm, and .pkg.tar.zst packages (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
}

/// The binaries `packaging/linux/nfpm.yaml` installs into `/usr/bin`.
///
/// One list drives both the build and the existence check below. They used to
/// be written out separately and drifted: the build stopped one short of the
/// package, so `openlogi-overlay` only ever reached a `.deb` when a cached
/// `target/release` happened to still hold one from an earlier run.
pub(super) const PACKAGED_BINS: [&str; 4] = [
    openlogi_core::brand::CLI_EXECUTABLE,
    openlogi_core::brand::GUI_EXECUTABLE,
    openlogi_core::brand::Helper::Overlay.executable(),
    openlogi_core::brand::Helper::Agent.executable(),
];

pub(crate) fn run(args: &Args) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);

    if !args.no_build {
        build_release_binaries(&sh)?;
    }
    ensure_release_binaries(&root)?;

    ensure_command("nfpm")?;

    let output = absolutize(&root, &args.output);
    let config = root.join("packaging/linux/nfpm.yaml");

    // nfpm stamps this into the package metadata and filename.
    let pkg_arch = HostArch::detect()?.nfpm();

    for packager in ["deb", "rpm", "archlinux"] {
        println!("==> nfpm {packager} ({pkg_arch})");
        cmd!(
            sh,
            "nfpm package --packager {packager} --config {config} --target {output}"
        )
        .env("VERSION", env!("CARGO_PKG_VERSION"))
        .env("PKG_ARCH", pkg_arch)
        .run()?;
    }

    println!();
    println!("Linux packages written to {}", output.display());
    Ok(())
}

/// `cargo build --release` every binary in [`PACKAGED_BINS`].
pub(super) fn build_release_binaries(sh: &Shell) -> Result<()> {
    println!("==> build release binaries");
    let packages: Vec<&str> = PACKAGED_BINS.iter().flat_map(|&bin| ["-p", bin]).collect();
    cmd!(sh, "cargo build --release {packages...}").run()?;
    Ok(())
}

/// Fail early when a binary [`PACKAGED_BINS`] expects is missing from
/// `target/release`, instead of letting the packager report a missing source.
pub(super) fn ensure_release_binaries(root: &Path) -> Result<()> {
    for bin in PACKAGED_BINS {
        ensure_file(&root.join("target/release").join(bin))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
