//! The `.tar.xz` Sparkle downloads as a full update, made from the stapled DMG.
//!
//! Runs on the macOS build leg after the DMG is stapled, next to `release
//! sparkle-deltas`. The DMG stays the download for a first install. An
//! update only needs the app, and a tar of it under xz is far smaller than
//! the DMG's per-chunk compression: v0.8.20 arm64 drops from 22.83 to
//! 14.17 MB (-38%). `release appcast` offers the archive in place of the DMG.
//!
//! The app comes out of the DMG through `ditto`, as for the deltas, so the
//! archive holds the bytes a DMG install puts on disk. Before the archive is
//! kept it is opened the way Sparkle opens it, `/usr/bin/tar -zxC` reading it
//! on stdin (bsdtar ignores `-z` when it extracts and detects xz itself). The
//! result must pass `codesign --verify --deep --strict` and match the app file
//! for file.
//!
//! An archive that cannot be made is a warning, not a failure: without one,
//! `release appcast` offers the DMG as before, which costs a larger download
//! and never an update.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use gpui_updater_release::sparkle;
use xshell::{Shell, cmd};

use crate::support::update_feed::MacArch;

/// xz preset for the archive. Level 9 is a 64 MiB dictionary, so one window
/// spans the whole 63 MB tar; it encodes in about 40 s. `-9e` is slower and
/// no smaller for this tar.
const XZ_PRESET: u32 = 9;

#[derive(Parser)]
pub(crate) struct Args {
    /// The stapled `OpenLogi-<tag>-macos-<arch>.dmg`. The archive is written
    /// next to it as `OpenLogi-<tag>-macos-<arch>.tar.xz`.
    #[arg(long)]
    dmg: PathBuf,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let dmg_name = args
        .dmg
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 file name", args.dmg.display()))?;
    MacArch::of_dmg(dmg_name)
        .with_context(|| format!("{dmg_name} is not an `…-macos-<arch>.dmg` release DMG"))?;
    let archive = args.dmg.with_file_name(archive_name(dmg_name)?);
    match make(&args.dmg, &archive) {
        Ok(()) => println!("{}", archive.display()),
        Err(error) => println!(
            "::warning::no Sparkle archive for {dmg_name}, so its feed offers the DMG: {error:#}"
        ),
    }
    Ok(())
}

/// The archive's file name for the DMG named `dmg`.
pub(crate) fn archive_name(dmg: &str) -> Result<String> {
    let stem = dmg
        .strip_suffix(".dmg")
        .with_context(|| format!("{dmg} is not a .dmg"))?;
    Ok(format!("{stem}.tar.xz"))
}

/// Copy the app out of `dmg`, archive it, check the archive, and only then
/// write it to `archive`, so a failure leaves nothing for `release appcast`.
fn make(dmg: &Path, archive: &Path) -> Result<()> {
    let sh = Shell::new()?;
    let scratch = tempfile::tempdir().context("could not create a scratch directory")?;
    let app = sparkle::extract_app(dmg, &scratch.path().join("app"), None)
        .with_context(|| format!("could not copy the app out of {}", dmg.display()))?;
    let (dir, name) = (
        app.parent().context("the app has no parent directory")?,
        app.file_name().context("the app has no name")?,
    );
    // No extended attributes, ACLs or AppleDouble files: the signature lives
    // in the bundle, and Sparkle's tar would only write them back.
    let tar = cmd!(
        sh,
        "/usr/bin/tar --no-mac-metadata --no-xattrs --no-acls -cf - -C {dir} {name}"
    )
    .quiet()
    .output()
    .context("could not tar the app")?
    .stdout;
    let xz = compress(&tar)?;

    let unpacked = scratch.path().join("check");
    fs_err::create_dir(&unpacked)?;
    cmd!(sh, "/usr/bin/tar -zxC {unpacked}")
        .stdin(&xz)
        .run()
        .context("Sparkle's `tar -zxC` could not extract the archive")?;
    let unpacked_app = unpacked.join(name);
    cmd!(sh, "codesign --verify --deep --strict {unpacked_app}")
        .run()
        .context("the app from the archive fails codesign")?;
    ensure_same_tree(&app, &unpacked_app)?;

    fs_err::write(archive, &xz)?;
    Ok(())
}

/// `tar` as one xz stream at [`XZ_PRESET`].
fn compress(tar: &[u8]) -> Result<Vec<u8>> {
    liblzma::encode_all(tar, XZ_PRESET).context("could not compress the tar")
}

/// One entry of a directory tree, as far as an installed app is concerned.
#[derive(Debug, PartialEq, Eq)]
enum Node {
    Dir { mode: u32 },
    File { mode: u32, bytes: Vec<u8> },
    Link { target: PathBuf },
}

/// Fail unless `actual` holds the same entries as `expected`: the same paths,
/// kinds, permission bits, file contents and link targets.
fn ensure_same_tree(expected: &Path, actual: &Path) -> Result<()> {
    let (want, got) = (tree(expected)?, tree(actual)?);
    for (path, node) in &want {
        match got.get(path) {
            None => bail!("{} is missing from the archive", path.display()),
            Some(other) if other != node => bail!("{} differs in the archive", path.display()),
            Some(_) => {}
        }
    }
    if let Some(extra) = got.keys().find(|path| !want.contains_key(*path)) {
        bail!("the archive adds {}", extra.display());
    }
    Ok(())
}

/// Every entry under `root`, keyed by its path relative to `root`. Links are
/// recorded, not followed.
fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Node>> {
    let mut nodes = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs_err::read_dir(&dir)? {
            let path = entry?.path();
            let metadata = fs_err::symlink_metadata(&path)?;
            let node = if metadata.file_type().is_symlink() {
                Node::Link {
                    target: fs_err::read_link(&path)?,
                }
            } else if metadata.is_dir() {
                pending.push(path.clone());
                Node::Dir {
                    mode: mode(&metadata),
                }
            } else {
                Node::File {
                    mode: mode(&metadata),
                    bytes: fs_err::read(&path)?,
                }
            };
            nodes.insert(path.strip_prefix(root)?.to_path_buf(), node);
        }
    }
    Ok(nodes)
}

/// Permission bits, which carry the executables' `x`.
#[cfg(unix)]
fn mode(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o7777
}

/// Windows has no permission bits; the archive is made on macOS only.
#[cfg(not(unix))]
fn mode(_metadata: &Metadata) -> u32 {
    0
}

#[cfg(test)]
mod tests;
