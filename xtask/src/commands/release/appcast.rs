//! A release's Sparkle feed record: one single-item appcast per macOS
//! architecture, offering that architecture's notarized DMG of this release,
//! signed with Sparkle's EdDSA key, plus the deltas `release sparkle-deltas`
//! made from earlier releases. A beta's item carries `sparkle:channel`.
//!
//! Runs in the publish job after the DMGs are stapled — stapling rewrites the
//! file, so a signature made before it would not match what users download.
//! The publish job uploads the record next to the release's artifacts, and
//! `release channels` copies its items, unchanged, into the feeds the bundles
//! read. Rendering and signing are `gpui-updater-release`'s; what OpenLogi's
//! feed says stays here.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use gpui_updater_release::sparkle::{self, SparkleDelta};
use gpui_updater_release::{
    Appcast, AppcastItem, Enclosure, ReleaseNotes, SparkleKey, ensure_sparkle_version,
};
use openlogi_core::update_feed::UpdateChannel;
use strum::VariantArray as _;
use time::{OffsetDateTime, format_description::well_known::Rfc2822};

use crate::support::release_tag::{BUILD_VERSION_ENV, BuildVersion, ReleaseTag};
use crate::support::update_feed::{self, MacArch};

/// The base64 private key: the 32-byte seed Sparkle's `generate_keys -x`
/// exports. Read only from the environment, so it never shows up in a
/// process listing.
const PRIVATE_KEY_ENV: &str = "OPENLOGI_SPARKLE_PRIVATE_ED_KEY";

#[derive(Parser)]
pub(crate) struct Args {
    /// Directory holding the stapled `…-macos-<arch>.dmg` release artifacts
    /// and the `<dmg>.from-<version>.sparkle.delta` files made from them.
    #[arg(long, default_value = "dist")]
    dist: PathBuf,
    /// Root of the tree to upload to the update host. The `appcast-<arch>.xml`
    /// records go into the release's feed record directory under it. Keep it
    /// out of `--dist`, whose contents are uploaded as release assets.
    #[arg(long, default_value = ".release/stage")]
    stage: PathBuf,
    /// Release tag, for example `v0.8.14`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// The bundles' `CFBundleVersion`, as `release version` printed it for
    /// the tag: `X.Y.Z` for a stable release, `B.N` for a beta.
    #[arg(long, env = BUILD_VERSION_ENV)]
    build_version: String,
    /// Public update base URL, for example `https://updates.openlogi.org`.
    #[arg(long, env = update_feed::BASE_URL_ENV)]
    base_url: String,
    /// The public key the bundles embed (`SUPublicEDKey`). Signing fails unless
    /// the private key belongs to it: a feed signed with any other key is one
    /// every installed app rejects.
    #[arg(long, env = update_feed::SPARKLE_PUBLIC_KEY_ENV)]
    public_key: String,
    /// Markdown release notes for Sparkle's update window.
    #[arg(long)]
    release_notes: Option<PathBuf>,
}

/// Everything an appcast item says about the release besides its enclosures.
struct Release<'a> {
    /// `CFBundleVersion` of the bundles in the DMGs, which Sparkle compares
    /// against the running app's. `macos package` stamps the build version
    /// there in place of cargo-bundle's build timestamp.
    bundle_version: &'a str,
    /// The version people read: the tag's, `X.Y.Z` or `X.Y.Z-beta.N`.
    version: &'a str,
    /// The channel the release publishes to.
    channel: UpdateChannel,
    pub_date: String,
    page_url: String,
    notes: Option<String>,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let private_key = std::env::var(PRIVATE_KEY_ENV).with_context(|| {
        format!("set {PRIVATE_KEY_ENV} to the base64 key `generate_keys -x` exports")
    })?;
    write_appcasts(args, &private_key)
}

fn write_appcasts(args: &Args, private_key: &str) -> Result<()> {
    let tag = ReleaseTag::parse(&args.tag)?;
    let build: BuildVersion = args.build_version.parse()?;
    build.ensure_fits(&tag)?;
    let bundle_version = build.to_string();
    ensure_sparkle_orders(&bundle_version, tag.channel())?;
    let key = signing_key(private_key)?;
    key.expect_public_key(&args.public_key).with_context(|| {
        format!("{PRIVATE_KEY_ENV} does not belong to the public key the bundles embed")
    })?;

    let notes = args
        .release_notes
        .as_deref()
        .map(|path| {
            fs_err::read_to_string(path)
                .with_context(|| format!("could not read release notes {}", path.display()))
        })
        .transpose()?;
    let version = tag.version().to_string();
    let release = Release {
        bundle_version: &bundle_version,
        version: &version,
        channel: tag.channel(),
        pub_date: OffsetDateTime::now_utc()
            .format(&Rfc2822)
            .context("could not format the publication date")?,
        page_url: update_feed::release_page_url(&tag),
        notes,
    };
    let release_base = update_feed::release_url(&args.base_url, &tag);
    let record_base = update_feed::record_url(&args.base_url, &tag);

    // Collect every record before writing any, so a bad delta leaves no
    // record half-written next to the others.
    let mut records = Vec::new();
    for (arch, dmg) in release_dmgs(&args.dist)? {
        let name = file_name(&dmg)?;
        let enclosure = Enclosure::for_file(&dmg, format!("{release_base}/{name}"), Some(&key))
            .with_context(|| format!("could not sign {}", dmg.display()))?;
        let mut item = item(&release, enclosure);
        for delta in release_deltas(&args.dist, name, &bundle_version)? {
            let url = format!("{release_base}/{}", file_name(&delta.path)?);
            item = item.delta(
                delta
                    .enclosure(url, Some(&key))
                    .with_context(|| format!("could not sign {}", delta.path.display()))?,
            );
        }
        let record_url = format!("{record_base}/{}", arch.appcast_file_name());
        records.push((arch, record(&record_url, &release, arch, item), record_url));
    }

    let output_dir = args.stage.join(update_feed::record_dir(&tag));
    fs_err::create_dir_all(&output_dir)
        .with_context(|| format!("could not create {}", output_dir.display()))?;
    for (arch, xml, record_url) in records {
        let path = output_dir.join(arch.appcast_file_name());
        fs_err::write(&path, xml).with_context(|| format!("could not write {}", path.display()))?;
        println!("{} → {}", path.display(), record_url);
    }
    Ok(())
}

/// Refuse a bundle version Sparkle would order wrongly. Its comparator
/// (`SUStandardVersionComparator`) stops at the first `-`, so it reads
/// `1.2.0-rc.1` as `1.2.0`: whoever took the pre-release would never be
/// offered the final 1.2.0. A stable release's bundle version is
/// `MAJOR.MINOR.PATCH`, and a beta's has a fourth numeric field.
fn ensure_sparkle_orders(bundle_version: &str, channel: UpdateChannel) -> Result<()> {
    let (parts, shape) = match channel {
        UpdateChannel::Stable => (3, "MAJOR.MINOR.PATCH"),
        UpdateChannel::Beta => (4, "MAJOR.MINOR.PATCH.N"),
    };
    ensure_sparkle_version(bundle_version, parts, parts).with_context(|| {
        format!(
            "a {} release's bundle version is {shape}, not {bundle_version:?}",
            channel.as_str()
        )
    })
}

/// The key behind Sparkle's exported private key: base64 of the 32-byte
/// Ed25519 seed. Sparkle 1's 96-byte export is an expanded key that no
/// longer yields its seed; re-export it with Sparkle 2's `generate_keys -x`.
fn signing_key(encoded: &str) -> Result<SparkleKey> {
    SparkleKey::from_base64(encoded).with_context(|| format!("{PRIVATE_KEY_ENV} is not usable"))
}

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 file name", path.display()))
}

/// The release DMG of every macOS architecture in `dist`, in [`MacArch`]
/// order. Both are required: the publish gate guarantees them, and a feed
/// left behind would keep offering that architecture the previous release.
fn release_dmgs(dist: &Path) -> Result<Vec<(MacArch, PathBuf)>> {
    let mut dmgs: Vec<(MacArch, PathBuf)> = Vec::new();
    for entry in fs_err::read_dir(dist)
        .with_context(|| format!("could not read artifact directory {}", dist.display()))?
    {
        let path = entry?.path();
        let Some(arch) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(MacArch::of_dmg)
        else {
            continue;
        };
        if let Some((_, other)) = dmgs.iter().find(|(seen, _)| *seen == arch) {
            bail!(
                "two {} DMGs: {} and {}",
                arch.as_str(),
                other.display(),
                path.display()
            );
        }
        dmgs.push((arch, path));
    }
    for arch in MacArch::VARIANTS {
        ensure!(
            dmgs.iter().any(|(seen, _)| seen == arch),
            "no {} DMG in {}",
            arch.as_str(),
            dist.display()
        );
    }
    dmgs.sort_by_key(|(arch, _)| MacArch::VARIANTS.iter().position(|a| a == arch));
    Ok(dmgs)
}

/// The Sparkle deltas in `dist` that update to the DMG named `dmg`, in file
/// name order. Each must carry the metadata `release sparkle-deltas` wrote
/// next to it, be named for the version it updates from, and produce
/// `bundle_version`: a delta to any other build would hand the app a bundle
/// the feed does not describe.
fn release_deltas(dist: &Path, dmg: &str, bundle_version: &str) -> Result<Vec<SparkleDelta>> {
    let prefix = format!("{dmg}.");
    let mut paths = Vec::new();
    for entry in fs_err::read_dir(dist)
        .with_context(|| format!("could not read artifact directory {}", dist.display()))?
    {
        let path = entry?.path();
        if sparkle::delta::is_delta_file(&path)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let delta = SparkleDelta::read_metadata(&path)
                .with_context(|| format!("could not read the metadata of {}", path.display()))?;
            let expected = sparkle::delta::delta_file_name(dmg, &delta.from.bundle_version);
            ensure!(
                file_name(&path)? == expected,
                "{} updates from {}, so it should be named {expected}",
                path.display(),
                delta.from.bundle_version
            );
            ensure!(
                delta.to_bundle_version == bundle_version,
                "{} updates to {}, not {bundle_version}",
                path.display(),
                delta.to_bundle_version
            );
            Ok(delta)
        })
        .collect()
}

/// The appcast item for `release`, offering `enclosure`.
fn item(release: &Release<'_>, enclosure: Enclosure) -> AppcastItem {
    let mut item = AppcastItem::new(release.bundle_version, enclosure)
        .title(format!("OpenLogi {}", release.version))
        .pub_date(Some(release.pub_date.clone()))
        .short_version(release.version)
        .minimum_system_version(update_feed::MACOS_MINIMUM_OS_VERSION)
        .link(&release.page_url)
        .full_release_notes_link(&release.page_url);
    // Sparkle offers an item without a channel to everyone, and one with a
    // channel only to apps that allow it.
    if release.channel != update_feed::APPCAST_CHANNEL {
        item = item.channel(release.channel.as_str());
    }
    match &release.notes {
        Some(notes) => item.notes(ReleaseNotes::Markdown(notes.clone())),
        None => item,
    }
}

/// `release`'s feed record for `arch`, at `record_url`: an appcast with the
/// one item.
fn record(record_url: &str, release: &Release<'_>, arch: MacArch, item: AppcastItem) -> String {
    Appcast::new("OpenLogi", record_url)
        .description(format!(
            "OpenLogi {} for {} (feed record)",
            release.version,
            arch.as_str()
        ))
        .item(item)
        .render()
}

#[cfg(test)]
mod tests;
