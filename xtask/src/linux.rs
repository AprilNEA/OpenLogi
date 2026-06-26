use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use xshell::{Shell, cmd};

use crate::util::{absolutize, ensure_command, ensure_file, repo_root};

#[derive(Parser)]
pub(crate) struct PackageLinux {
    /// Output directory for .deb and .rpm packages (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
}

pub(crate) fn package_linux(args: &PackageLinux) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);

    if !args.no_build {
        println!("==> build release binaries");
        cmd!(
            sh,
            "cargo build --release -p openlogi -p openlogi-gui -p openlogi-agent"
        )
        .run()?;
    }

    for bin in ["openlogi", "openlogi-gui", "openlogi-agent"] {
        ensure_file(&root.join("target/release").join(bin))?;
    }

    ensure_command("nfpm")?;

    let output = absolutize(&root, &args.output);
    let config = root.join("packaging/linux/nfpm.yaml");

    // nfpm stamps this into the package metadata and filename. The release CI
    // builds natively on an amd64 and an arm64 runner, so the host arch is the
    // package arch — map Rust's arch names to nfpm's.
    let pkg_arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => anyhow::bail!("unsupported Linux package architecture: {other}"),
    };

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
