use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;
use xshell::{Shell, cmd};

use crate::support::fs::{absolutize, ensure_command, ensure_file, repo_root};

#[derive(Parser)]
pub(crate) struct Args {
    /// Output directory for .deb, .rpm, .pkg.tar.zst, and .tar.gz packages
    /// (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
}

pub(crate) fn run(args: &Args) -> Result<()> {
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
    ensure_command("tar")?;

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

    build_tarball(&root, &output, pkg_arch)?;

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

fn build_tarball(root: &Path, output: &Path, pkg_arch: &str) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let package_dir_name = format!("openlogi-{version}-linux-{pkg_arch}");
    let tmp = tempfile::tempdir().context("could not create temp dir for Linux tarball")?;
    let package_dir = tmp.path().join(&package_dir_name);

    println!("==> tar.gz ({pkg_arch})");

    for sub in ["bin", "packaging/linux", "design/icon", "docs"] {
        fs_err::create_dir_all(package_dir.join(sub))
            .with_context(|| format!("could not create {}", package_dir.join(sub).display()))?;
    }

    for bin in ["openlogi", "openlogi-gui", "openlogi-agent"] {
        copy_file(
            &root.join("target/release").join(bin),
            &package_dir.join("bin").join(bin),
        )?;
    }

    copy_dir(
        &root.join("packaging/linux"),
        &package_dir.join("packaging/linux"),
    )?;
    copy_file(
        &root.join("design/icon/openlogi.png"),
        &package_dir.join("design/icon/openlogi.png"),
    )?;

    for file in [
        "README.md",
        "CHANGELOG.md",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "docs/INSTALL-linux.md",
    ] {
        copy_file(&root.join(file), &package_dir.join(file))?;
    }

    let archive = output.join(format!("{package_dir_name}.tar.gz"));
    let sh = Shell::new()?;
    let tmp_path = tmp.path();
    cmd!(sh, "tar -czf {archive} -C {tmp_path} {package_dir_name}").run()?;

    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs_err::create_dir_all(dst).with_context(|| format!("could not create {}", dst.display()))?;

    for entry in fs_err::read_dir(src).with_context(|| format!("could not read {}", src.display()))?
    {
        let entry = entry.with_context(|| format!("could not read entry in {}", src.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("could not inspect {}", entry.path().display()))?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if file_type.is_file() {
            copy_file(&entry.path(), &target)?;
        }
    }

    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs_err::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs_err::copy(src, dst)
        .with_context(|| format!("could not copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}
