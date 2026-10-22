//! The Sparkle appcasts: one feed per macOS architecture, each offering that
//! architecture's notarized DMG of this release, signed with Sparkle's EdDSA
//! key, plus the deltas `release sparkle-deltas` made from earlier releases.
//!
//! Runs in the publish job after the DMGs are stapled — stapling rewrites the
//! file, so a signature made before it would not match what users download.
//! The bundles read these feeds from the URLs `support::update_feed` gives
//! them, and the publish job uploads them there. Rendering and signing are
//! `gpui-updater-release`'s; what OpenLogi's feed says stays here.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use gpui_updater_release::sparkle::{self, SparkleDelta};
use gpui_updater_release::{
    Appcast, AppcastItem, Enclosure, ReleaseNotes, SparkleKey, ensure_sparkle_version,
};
use strum::VariantArray as _;
use time::{OffsetDateTime, format_description::well_known::Rfc2822};

use crate::support::release_tag::ReleaseTag;
use crate::support::update_feed::{self, MacArch};

/// The base64 private key: the 32-byte seed Sparkle's `generate_keys -x`
/// exports. Read only from the environment, so it never shows up in a
/// process listing.
const PRIVATE_KEY_ENV: &str = "OPENLOGI_SPARKLE_PRIVATE_ED_KEY";

/// The extension of the deltas `release sparkle-deltas` writes; the rest of
/// the name is `sparkle::delta::delta_file_name`'s.
const DELTA_EXTENSION: &str = "sparkle-delta";

#[derive(Parser)]
pub(crate) struct Args {
    /// Directory holding the stapled `…-macos-<arch>.dmg` release artifacts
    /// and the `<dmg>.from-<version>.sparkle-delta` files made from them.
    #[arg(long, default_value = "dist")]
    dist: PathBuf,
    /// Directory the `appcast-<arch>.xml` feeds are written to. Keep it out
    /// of `--dist`, whose contents are uploaded as release assets.
    #[arg(long, default_value = ".release/appcast")]
    output_dir: PathBuf,
    /// Release tag, for example `v0.8.14`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
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
    /// against the running app's. `macos package` stamps the crate version
    /// there in place of cargo-bundle's build timestamp.
    version: &'a str,
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
    let version = ReleaseTag::parse(&args.tag)?.version().to_string();
    ensure_sparkle_orders(&version)?;
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
    let release = Release {
        version: &version,
        pub_date: OffsetDateTime::now_utc()
            .format(&Rfc2822)
            .context("could not format the publication date")?,
        page_url: format!(
            "https://github.com/AprilNEA/OpenLogi/releases/tag/{}",
            args.tag
        ),
        notes,
    };
    let release_base = format!(
        "{}/releases/{}",
        args.base_url.trim_end_matches('/'),
        args.tag
    );

    // Collect every feed before writing any, so a bad delta leaves no feed
    // half-updated next to the others.
    let mut feeds = Vec::new();
    for (arch, dmg) in release_dmgs(&args.dist)? {
        let name = file_name(&dmg)?;
        let enclosure = Enclosure::for_file(&dmg, format!("{release_base}/{name}"), Some(&key))
            .with_context(|| format!("could not sign {}", dmg.display()))?;
        let mut item = item(&release, enclosure);
        for delta in release_deltas(&args.dist, name, &version)? {
            let url = format!("{release_base}/{}", file_name(&delta.path)?);
            item = item.delta(
                delta
                    .enclosure(url, Some(&key))
                    .with_context(|| format!("could not sign {}", delta.path.display()))?,
            );
        }
        let feed_url = update_feed::appcast_url(&args.base_url, arch);
        feeds.push((arch, appcast(&feed_url, item), feed_url));
    }

    fs_err::create_dir_all(&args.output_dir)
        .with_context(|| format!("could not create {}", args.output_dir.display()))?;
    for (arch, xml, feed_url) in feeds {
        let feed = args.output_dir.join(arch.appcast_file_name());
        fs_err::write(&feed, xml).with_context(|| format!("could not write {}", feed.display()))?;
        println!("{} → {}", feed.display(), feed_url);
    }
    Ok(())
}

/// Refuse a version Sparkle would order wrongly. Its comparator
/// (`SUStandardVersionComparator`) stops at the first `-`, so it reads
/// `1.2.0-rc.1` as `1.2.0`: whoever took the pre-release from the stable feed
/// would never be offered the final 1.2.0.
fn ensure_sparkle_orders(version: &str) -> Result<()> {
    ensure_sparkle_version(version, 3, 3).with_context(|| {
        format!("the stable appcast only carries MAJOR.MINOR.PATCH releases, not {version:?}")
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
/// `version`: a delta to any other build would hand the app a bundle the feed
/// does not describe.
fn release_deltas(dist: &Path, dmg: &str, version: &str) -> Result<Vec<SparkleDelta>> {
    let prefix = format!("{dmg}.");
    let mut paths = Vec::new();
    for entry in fs_err::read_dir(dist)
        .with_context(|| format!("could not read artifact directory {}", dist.display()))?
    {
        let path = entry?.path();
        let is_delta = path.extension().is_some_and(|ext| ext == DELTA_EXTENSION);
        if is_delta
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
                delta.to_bundle_version == version,
                "{} updates to {}, not {version}",
                path.display(),
                delta.to_bundle_version
            );
            Ok(delta)
        })
        .collect()
}

/// The appcast item for `release`, offering `enclosure`.
fn item(release: &Release<'_>, enclosure: Enclosure) -> AppcastItem {
    let item = AppcastItem::new(release.version, enclosure)
        .title(format!("OpenLogi {}", release.version))
        .pub_date(Some(release.pub_date.clone()))
        .short_version(release.version)
        .minimum_system_version(update_feed::MACOS_MINIMUM_OS_VERSION)
        .link(&release.page_url)
        .full_release_notes_link(&release.page_url);
    match &release.notes {
        Some(notes) => item.notes(ReleaseNotes::Markdown(notes.clone())),
        None => item,
    }
}

/// The appcast at `feed_url`: one item.
fn appcast(feed_url: &str, item: AppcastItem) -> String {
    Appcast::new("OpenLogi", feed_url)
        .description(format!(
            "OpenLogi updates ({} channel)",
            update_feed::CHANNEL.as_str()
        ))
        .item(item)
        .render()
}

#[cfg(test)]
mod tests;
