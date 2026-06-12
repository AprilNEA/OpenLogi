use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context as _, Result};
use clap::Parser;

use crate::util::{TempDir, absolutize, ensure_command, ensure_file, repo_root, run};

#[derive(Parser)]
pub(crate) struct PackageLinux {
    /// Output directory for .deb, .rpm, and .tar.gz packages (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
}

pub(crate) fn package_linux(args: &PackageLinux) -> Result<()> {
    let root = repo_root()?;

    if !args.no_build {
        println!("==> build release binaries");
        run(ProcessCommand::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                "openlogi",
                "-p",
                "openlogi-gui",
                "-p",
                "openlogi-agent",
            ])
            .current_dir(&root))?;
    }

    for bin in ["openlogi", "openlogi-gui", "openlogi-agent"] {
        ensure_file(&root.join("target/release").join(bin))?;
    }

    let output = absolutize(&root, &args.output);
    fs::create_dir_all(&output)
        .with_context(|| format!("could not create output directory {}", output.display()))?;
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

    ensure_command("nfpm")?;

    for packager in ["deb", "rpm"] {
        println!("==> nfpm {packager} ({pkg_arch})");
        run(ProcessCommand::new("nfpm")
            .args(["package", "--packager", packager, "--config"])
            .arg(&config)
            .arg("--target")
            .arg(&output)
            .env("VERSION", env!("CARGO_PKG_VERSION"))
            .env("PKG_ARCH", pkg_arch)
            .current_dir(&root))?;
    }

    println!();
    println!("Linux packages written to {}", output.display());
    Ok(())
}

fn build_tarball(root: &Path, output: &Path, pkg_arch: &str) -> Result<()> {
    ensure_command("tar")?;

    let version = env!("CARGO_PKG_VERSION");
    let package_dir_name = format!("openlogi-{version}-linux-{pkg_arch}");
    let tmp = TempDir::new("openlogi-linux-package")?;
    let package_dir = tmp.path().join(&package_dir_name);

    println!("==> tar.gz ({pkg_arch})");

    fs::create_dir_all(package_dir.join("bin"))
        .with_context(|| format!("could not create {}", package_dir.join("bin").display()))?;
    fs::create_dir_all(package_dir.join("packaging/linux")).with_context(|| {
        format!(
            "could not create {}",
            package_dir.join("packaging/linux").display()
        )
    })?;
    fs::create_dir_all(package_dir.join("design/icon")).with_context(|| {
        format!(
            "could not create {}",
            package_dir.join("design/icon").display()
        )
    })?;
    fs::create_dir_all(package_dir.join("docs"))
        .with_context(|| format!("could not create {}", package_dir.join("docs").display()))?;

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
    run(ProcessCommand::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(tmp.path())
        .arg(&package_dir_name))?;

    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("could not create {}", dst.display()))?;

    for entry in fs::read_dir(src).with_context(|| format!("could not read {}", src.display()))? {
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
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("could not copy {} to {}", src.display(), dst.display()))?;

    let permissions = fs::metadata(src)
        .with_context(|| format!("could not read metadata for {}", src.display()))?
        .permissions();
    fs::set_permissions(dst, permissions)
        .with_context(|| format!("could not set permissions on {}", dst.display()))?;

    Ok(())
}
