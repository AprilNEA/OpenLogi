use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use xshell::{Shell, cmd};

use crate::support::fs::{absolutize, ensure_command, ensure_file, repo_root};

mod payload;

#[derive(Parser)]
pub(crate) struct Args {
    /// Output directory for .deb, .rpm, and .pkg.tar.zst packages (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
}

/// The package name `packaging/linux/nfpm.yaml` declares, which starts every
/// package's file name.
const PACKAGE_NAME: &str = "openlogi";

/// The binaries `packaging/linux/nfpm.yaml` installs into `/usr/bin`.
///
/// One list drives both the build and the existence check below. They used to
/// be written out separately and drifted: the build stopped one short of the
/// package, so `openlogi-overlay` only ever reached a `.deb` when a cached
/// `target/release` happened to still hold one from an earlier run.
const PACKAGED_BINS: [&str; 4] = [
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
        println!("==> build release binaries");
        let packages: Vec<&str> = PACKAGED_BINS.iter().flat_map(|&bin| ["-p", bin]).collect();
        cmd!(sh, "cargo build --release {packages...}").run()?;
    }

    for bin in PACKAGED_BINS {
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

    let version = env!("CARGO_PKG_VERSION");
    fs_err::create_dir_all(&output)?;
    // nfpm names each package itself. Writing it alone into a fresh directory
    // makes the file it wrote the only one there, whatever an earlier run left
    // in `output`, and keeps a half-rewritten package out of `output`.
    let staging = tempfile::tempdir_in(&output)?;
    for packager in ["deb", "rpm", "archlinux"] {
        println!("==> nfpm {packager} ({pkg_arch})");
        let staged = staging.path().join(packager);
        fs_err::create_dir(&staged)?;
        cmd!(
            sh,
            "nfpm package --packager {packager} --config {config} --target {staged}"
        )
        .env("VERSION", nfpm_version(packager, version))
        .env("PKG_ARCH", pkg_arch)
        .run()?;
        let written = fs_err::read_dir(&staged)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        let [package] = written.as_slice() else {
            bail!(
                "expected nfpm to write one {packager} package, found {}",
                written.len()
            );
        };
        match packager {
            "deb" => {
                println!("==> recompress {} (xz)", package.display());
                payload::recompress_deb(package)?;
            }
            "archlinux" => {
                println!("==> recompress {} (zstd, long window)", package.display());
                payload::recompress_arch(package)?;
            }
            _ => {}
        }
        let name = package
            .file_name()
            .context("nfpm wrote a package without a file name")?;
        fs_err::rename(package, output.join(name))?;
    }
    check_arch_pkgver(&sh, &output, &nfpm_version("archlinux", version))?;

    println!();
    println!("Linux packages written to {}", output.display());
    Ok(())
}

/// The version to hand nfpm for `packager`.
///
/// nfpm splits a semver pre-release off the version. Debian and RPM keep it
/// (`0.8.18~beta.1`), and so does the Arch file name, but nfpm 2.46.3 writes
/// it into the Arch `.PKGINFO` only when the package has an epoch. Without
/// one, `0.8.18-beta.1` would declare `pkgver = 0.8.18-1`, the same as the
/// final release. Arch spells a pre-release without the hyphen
/// (`0.8.18beta.1`), which is not semver, so nfpm keeps it whole. pacman
/// sorts it before `0.8.18`, and the file name stays the same.
fn nfpm_version(packager: &str, version: &str) -> String {
    match (packager, version.split_once('-')) {
        ("archlinux", Some((release, pre))) => format!("{release}{}", pre.replace('-', "_")),
        _ => version.to_owned(),
    }
}

/// Fail unless the Arch package in `output` declares `version`.
///
/// pacman compares `pkgver` in `.PKGINFO`, not the file name, and nfpm has
/// written the two differently (see [`nfpm_version`]).
fn check_arch_pkgver(sh: &Shell, output: &Path, version: &str) -> Result<()> {
    let prefix = format!("{PACKAGE_NAME}-{version}-");
    let mut packages = Vec::new();
    for entry in fs_err::read_dir(output)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".pkg.tar.zst") {
            packages.push(output.join(&*name));
        }
    }
    let [package] = packages.as_slice() else {
        bail!(
            "expected one Arch package {prefix}*.pkg.tar.zst in {}, found {}",
            output.display(),
            packages.len()
        );
    };
    let pkginfo = cmd!(sh, "tar -xOf {package} .PKGINFO")
        .read()
        .context("reading .PKGINFO back needs tar with zstd")?;
    let declared = pkgver(&pkginfo);
    ensure!(
        declared
            .and_then(|pkgver| pkgver.rsplit_once('-'))
            .map(|(pkgver, _)| pkgver)
            == Some(version),
        "{} declares pkgver {declared:?}, not {version}-<pkgrel>",
        package.display()
    );
    Ok(())
}

/// The `pkgver` line of a `.PKGINFO`.
fn pkgver(pkginfo: &str) -> Option<&str> {
    pkginfo
        .lines()
        .find_map(|line| line.strip_prefix("pkgver = "))
}

#[cfg(test)]
mod tests;
