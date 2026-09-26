//! `cargo xtask linux appimage`: the four release binaries, the desktop entry,
//! the icons, the udev rule, and the systemd unit, wrapped by `appimagetool`
//! into one self-contained executable.
//!
//! The AppDir is not a second content list. It is staged straight from
//! `packaging/linux/nfpm.yaml`, each `dst` re-rooted under the AppDir, so the
//! AppImage carries exactly what the `.deb`/`.rpm` install and a file added to
//! one cannot go missing from the other. Only the AppImage-specific top level
//! (`AppRun`, the desktop entry and icon appimagetool reads) is added here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use xshell::{Shell, cmd};

use super::arch::HostArch;
use super::package::{build_release_binaries, ensure_release_binaries};
use crate::support::fs::{absolutize, ensure_command, ensure_file, repo_root};

#[derive(Parser)]
pub(crate) struct Args {
    /// Output directory for the .AppImage (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
    /// AppImage runtime to embed. Without it appimagetool downloads the current
    /// type2 runtime at package time; CI pins one and passes it here.
    #[arg(long, env = "OPENLOGI_APPIMAGE_RUNTIME")]
    runtime_file: Option<PathBuf>,
}

/// The one description of what a Linux install contains.
const NFPM_CONFIG: &str = "packaging/linux/nfpm.yaml";
/// The entry point the AppImage runtime executes.
const APP_RUN: &str = "packaging/linux/appimage/AppRun";
/// Where the staged AppDir lives between runs, for inspection.
const APPDIR: &str = "target/appimage/OpenLogi.AppDir";

/// Where nfpm installs launchers; the AppDir needs that entry at its root too.
const APPLICATIONS_DIR: &str = "/usr/share/applications/";
/// The icon size AppImage tooling expects at the AppDir root and as `.DirIcon`.
const DIR_ICON_DIR: &str = "/usr/share/icons/hicolor/256x256/apps/";

pub(crate) fn run(args: &Args) -> Result<()> {
    let root = repo_root()?;
    let sh = Shell::new()?;
    let _repo = sh.push_dir(&root);

    if !args.no_build {
        build_release_binaries(&sh)?;
    }
    ensure_release_binaries(&root)?;
    ensure_command("appimagetool")?;

    let arch = HostArch::detect()?.appimage();
    let appdir = root.join(APPDIR);
    println!("==> stage {}", appdir.display());
    let config = fs_err::read_to_string(root.join(NFPM_CONFIG))?;
    stage_appdir(&root, &appdir, &nfpm_contents(&config)?)?;

    let output_dir = absolutize(&root, &args.output);
    fs_err::create_dir_all(&output_dir)?;
    let output = output_dir.join(format!(
        "{}-{}-{arch}.AppImage",
        openlogi_core::brand::APP_NAME,
        env!("CARGO_PKG_VERSION")
    ));
    if output.exists() {
        fs_err::remove_file(&output)?;
    }

    let mut runtime_args: Vec<OsString> = Vec::new();
    if let Some(runtime) = &args.runtime_file {
        let runtime = absolutize(&root, runtime);
        ensure_file(&runtime)?;
        runtime_args.push("--runtime-file".into());
        runtime_args.push(runtime.into());
    }

    println!("==> appimagetool ({arch})");
    // No AppStream metainfo ships yet, and appimagetool treats its absence as
    // an error rather than a warning without this flag.
    cmd!(
        sh,
        "appimagetool --no-appstream {runtime_args...} {appdir} {output}"
    )
    .env("ARCH", arch)
    .run()?;

    println!();
    println!("AppImage written to {}", output.display());
    Ok(())
}

/// One `contents:` entry of `nfpm.yaml`: a repository-relative source and the
/// absolute path a package installs it to.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Entry<'a> {
    pub(super) src: &'a str,
    pub(super) dst: &'a str,
}

/// The `src`/`dst` pairs of `nfpm.yaml`'s `contents:` list.
///
/// A line-level read of a file this repository owns and keeps flat, the same
/// way the package tests check it, rather than a YAML dependency for two keys.
/// Every entry must carry both keys and `dst` must be absolute, so a malformed
/// manifest fails here instead of staging a file somewhere surprising.
pub(super) fn nfpm_contents(config: &str) -> Result<Vec<Entry<'_>>> {
    let mut entries = Vec::new();
    let mut pending: Option<&str> = None;
    for (index, line) in config.lines().enumerate() {
        let line = line.trim();
        let number = index + 1;
        if let Some(src) = line.strip_prefix("- src:") {
            if let Some(previous) = pending {
                bail!("{NFPM_CONFIG}:{number}: `src: {previous}` has no `dst:`");
            }
            pending = Some(src.trim());
        } else if let Some(dst) = line.strip_prefix("dst:") {
            let src = pending
                .take()
                .with_context(|| format!("{NFPM_CONFIG}:{number}: `dst:` without a `src:`"))?;
            let dst = dst.trim();
            if !dst.starts_with('/') {
                bail!("{NFPM_CONFIG}:{number}: `dst: {dst}` is not absolute");
            }
            entries.push(Entry { src, dst });
        }
    }
    if let Some(src) = pending {
        bail!("{NFPM_CONFIG}: `src: {src}` has no `dst:`");
    }
    if entries.is_empty() {
        bail!("{NFPM_CONFIG}: no `contents:` entries found");
    }
    Ok(entries)
}

/// The staged file that the AppDir root must also expose, found by the
/// directory nfpm installs it into.
fn single_entry_under<'a>(entries: &[Entry<'a>], dir: &str) -> Result<&'a str> {
    let mut matches = entries.iter().filter(|entry| entry.dst.starts_with(dir));
    let found = matches
        .next()
        .with_context(|| format!("{NFPM_CONFIG} installs nothing under {dir}"))?;
    if matches.next().is_some() {
        bail!("{NFPM_CONFIG} installs more than one file under {dir}");
    }
    Ok(found.dst)
}

/// Rebuild `appdir` from scratch: every nfpm entry re-rooted under it, plus
/// `AppRun` and the root-level desktop entry and icon appimagetool reads.
fn stage_appdir(root: &Path, appdir: &Path, entries: &[Entry<'_>]) -> Result<()> {
    if appdir.exists() {
        fs_err::remove_dir_all(appdir)?;
    }
    fs_err::create_dir_all(appdir)?;

    for entry in entries {
        let relative = entry
            .dst
            .strip_prefix('/')
            .with_context(|| format!("`dst: {}` is not absolute", entry.dst))?;
        let target = appdir.join(relative);
        if let Some(parent) = target.parent() {
            fs_err::create_dir_all(parent)?;
        }
        fs_err::copy(root.join(entry.src), &target)?;
    }

    let app_run = appdir.join("AppRun");
    fs_err::copy(root.join(APP_RUN), &app_run)?;
    make_executable(&app_run)?;

    let desktop = single_entry_under(entries, APPLICATIONS_DIR)?;
    let desktop_name = file_name(desktop)?;
    symlink(desktop.trim_start_matches('/'), &appdir.join(desktop_name))?;

    let icon = single_entry_under(entries, DIR_ICON_DIR)?;
    let icon_name = file_name(icon)?;
    symlink(icon.trim_start_matches('/'), &appdir.join(icon_name))?;
    symlink(icon_name, &appdir.join(".DirIcon"))?;
    Ok(())
}

fn file_name(path: &str) -> Result<&str> {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .with_context(|| format!("`dst: {path}` names no file"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mut permissions = fs_err::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs_err::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn symlink(target: impl AsRef<Path>, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target.as_ref(), link).with_context(|| {
        format!(
            "could not link {} -> {}",
            link.display(),
            target.as_ref().display()
        )
    })
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    bail!("an AppImage can only be assembled on a Unix host")
}

#[cfg(not(unix))]
fn symlink(_target: impl AsRef<Path>, _link: &Path) -> Result<()> {
    bail!("an AppImage can only be assembled on a Unix host")
}

#[cfg(test)]
mod tests;
