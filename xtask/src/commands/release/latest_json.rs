//! `release latest-json`: the built-in update engine's manifest for one
//! release, written into the release's feed record.
//!
//! gpui-updater-release's `ManifestBuilder` scans `--dist`, hashes and sizes
//! each artifact, pairs it with its `.minisig`, and writes the manifest the
//! engine reads (`gpui_updater_core::manifest`). What stays here is
//! OpenLogi's: which artifacts the in-app updater installs, the minimum OS
//! each one advertises, and the Windows gate.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Parser;
use gpui_updater_release::{
    ArtifactRule, Classifier, Manifest, ManifestBuilder, SignaturePolicy, write_manifest,
};

// The manifest advertises the identity the shipped bundle is stamped with, so
// both come from one constant.
use openlogi_core::brand::APP_ID;

use crate::support::release_tag::ReleaseTag;
use crate::support::update_feed::{self, BASE_URL_ENV, KEYS_ONLY, MACOS_MINIMUM_OS_VERSION};

/// Windows 10+. Informational — the client updater doesn't gate on it today,
/// and everything that can run OpenLogi reports at least 10.0.
const WINDOWS_MINIMUM_OS_VERSION: &str = "10.0";

/// The architectures a release ships, as its artifact names spell them after
/// the `-<os>-` marker. A name without one (a universal build, a foreign
/// archive) is not an updater artifact.
const ARCHES: [&str; 2] = ["arm64", "x86_64"];

#[derive(Parser)]
pub(crate) struct Args {
    /// Directory containing release artifacts.
    #[arg(long, default_value = "dist")]
    dist: PathBuf,
    /// Root of the tree to upload to the update host. The manifest goes into
    /// the release's feed record directory under it. Keep it out of
    /// `--dist`, whose contents are uploaded as release assets.
    #[arg(long, default_value = ".release/stage")]
    stage: PathBuf,
    /// Release tag, for example `v0.2.0`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// Public update base URL, for example `https://updates.openlogi.org`.
    #[arg(long, env = BASE_URL_ENV)]
    base_url: String,
    /// Also emit the per-arch Windows `.msi`/`.zip` entries. Off by default so
    /// the manifest can never reference objects the release workflow's R2
    /// upload step doesn't ship. The release workflow passes it and ships the
    /// zip/msi to the `releases/` prefix; the Windows client downloads the
    /// `.msi` and installs it via gpui-updater's staged msiexec flow.
    #[arg(long)]
    include_windows: bool,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let tag = ReleaseTag::parse(&args.tag)?;
    let manifest = build(args, &tag)?;
    let output = args
        .stage
        .join(update_feed::record_layout(KEYS_ONLY, &tag).manifest_path());
    write_manifest(&manifest, &output)
        .with_context(|| format!("could not write manifest to {}", output.display()))?;
    println!("{}", output.display());
    Ok(())
}

/// `tag`'s manifest from the artifacts in `args.dist`: every updater artifact
/// with its minisign signature URL, sorted by name.
fn build(args: &Args, tag: &ReleaseTag) -> Result<Manifest> {
    let layout = update_feed::channel_layout(&args.base_url, tag.channel());
    let mut builder = ManifestBuilder::for_layout(&layout, &tag.to_string())
        .app_id(APP_ID)
        .release_url(update_feed::release_page_url(tag))
        .classifier(classifier())
        // Every updater artifact ships its `.minisig`; one without is a broken
        // release, not an artifact to list unsigned.
        .signatures(SignaturePolicy::Required)
        // The DMGs are the publish gate's guaranteed artifact set; the Windows
        // legs are best-effort per arch (a failed leg publishes without them),
        // so their absence must not sink the whole manifest.
        .require_os("macos");
    if !args.include_windows {
        // Gated so the manifest and the R2 upload step can never disagree
        // about the Windows artifacts — see the `include_windows` arg doc.
        builder = builder.exclude_os("windows");
    }
    builder
        .scan_dir(&args.dist)
        .with_context(|| format!("could not read artifact directory {}", args.dist.display()))?
        .build()
        .with_context(|| format!("could not build {tag}'s update manifest"))
}

/// The artifacts the in-app updater installs. The Linux packages
/// (`.deb`/`.rpm`) are deliberately absent: those installs update through the
/// distro package manager. So are the Windows `.exe` installer, which the R2
/// upload skips, and everything else in `--dist` (`SHA256SUMS`, the
/// signatures themselves, the Sparkle deltas).
fn classifier() -> Classifier {
    Classifier::empty()
        .rule(
            ArtifactRule::new(".dmg", "macos", "dmg")
                .content_type("application/x-apple-diskimage")
                .minimum_os_version(MACOS_MINIMUM_OS_VERSION)
                .arches(ARCHES),
        )
        .rule(
            ArtifactRule::new(".msi", "windows", "msi")
                .content_type("application/x-msi")
                .minimum_os_version(WINDOWS_MINIMUM_OS_VERSION)
                .arches(ARCHES),
        )
        .rule(
            ArtifactRule::new(".zip", "windows", "zip")
                .content_type("application/zip")
                .minimum_os_version(WINDOWS_MINIMUM_OS_VERSION)
                .arches(ARCHES),
        )
}

/// `manifest` as the update host serves it: what [`write_manifest`] writes.
pub(super) fn render(manifest: &Manifest) -> Result<String> {
    Ok(serde_json::to_string_pretty(manifest)? + "\n")
}

#[cfg(test)]
mod tests;
