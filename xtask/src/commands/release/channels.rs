//! The channel files: what each [`UpdateChannel`] offers, composed from the
//! release records on the update host.
//!
//! Every release uploads its own feed record before this runs: `release
//! latest-json` and `release appcast` write it. This command reads a copy of
//! the host's records, picks the newest stable release and the newest beta,
//! and writes:
//!
//! - the stable channel's `latest.json`: the newest stable release's manifest;
//! - the beta channel's `latest.json`: the newest beta's manifest when the beta
//!   is newer than the newest stable release, and the stable release's
//!   otherwise, with its `channel` field naming the beta channel;
//! - one Sparkle feed per architecture in [`APPCAST_CHANNEL`]'s directory: the
//!   newest stable release's item, and before it the newest beta's when the
//!   beta is newer. Sparkle offers the beta item only to apps that allow the
//!   beta channel.
//!
//! "Newer" is Sparkle's order of build versions, which follows the commits: a
//! beta is newer than a stable release only when it was cut after it. A beta
//! numbered past a stable release but cut before it (the release PR's version
//! went down in between) is older, and the stable release has its changes.
//!
//! Items are copied from the records byte for byte, so their signatures stay
//! the ones `release appcast` made. The files are recomputed from every record
//! each time, so a release that publishes out of order still leaves each
//! channel on its newest release.
//!
//! Records began after `v0.8.17`. Until a stable release has one, the stable
//! channel's current files stand in as the newest stable release's record.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Parser;
use gpui_updater_release::Appcast;
use gpui_updater_release::appcast::SPARKLE_NAMESPACE;
use openlogi_core::update_feed::{MANIFEST_FILE_NAME, UpdateChannel};
use strum::VariantArray as _;

use super::latest_json::Manifest;
use crate::support::github_output;
use crate::support::release_tag::{BuildVersion, ReleaseTag};
use crate::support::update_feed::{self, APPCAST_CHANNEL, MacArch};

/// How every feed `Appcast::render` writes ends. Items go right before it.
const FEED_END: &str = "  </channel>\n</rss>\n";

#[derive(Parser)]
pub(crate) struct Args {
    /// The release tag being published, for example `v0.8.18-beta.3`. Its
    /// record must already be in `--mirror`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// A copy of the update host's JSON and XML files, laid out as on the
    /// host: the release records and the current channel files.
    #[arg(long, default_value = ".release/bucket")]
    mirror: PathBuf,
    /// Root of the tree to upload to the update host. The channel files go
    /// into their channel directories under it. It must be empty or missing,
    /// so nothing stale is uploaded with them.
    #[arg(long, default_value = ".release/channels")]
    output_dir: PathBuf,
    /// Public update base URL, for example `https://updates.openlogi.org`.
    #[arg(long, env = update_feed::BASE_URL_ENV)]
    base_url: String,
}

/// A release's feed record.
#[derive(Debug)]
struct Record {
    tag: ReleaseTag,
    build: BuildVersion,
    manifest: Manifest,
    /// The manifest file as the host serves it.
    manifest_text: String,
    /// Each architecture's item as its appcast spells it, in [`MacArch`]
    /// order.
    items: Vec<String>,
}

/// What the channels offer.
#[derive(Debug)]
struct Selection<'a> {
    /// The newest stable release.
    stable: &'a Record,
    /// The newest beta, when it is newer than [`Self::stable`].
    beta: Option<&'a Record>,
}

impl Selection<'_> {
    /// The release the beta channel offers.
    fn beta_channel(&self) -> &Record {
        self.beta.unwrap_or(self.stable)
    }
}

/// What [`write_channels`] wrote.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    stable_tag: String,
    beta_tag: String,
    /// The version the beta channel offers.
    beta_version: String,
    /// Whether the beta channel offered another version before.
    beta_changed: bool,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let outcome = write_channels(args)?;
    println!("stable channel: {}", outcome.stable_tag);
    println!(
        "beta channel: {}{}",
        outcome.beta_tag,
        if outcome.beta_changed {
            ""
        } else {
            " (unchanged)"
        }
    );
    github_output::write(&[
        ("stable_tag", &outcome.stable_tag),
        ("beta_tag", &outcome.beta_tag),
        ("beta_version", &outcome.beta_version),
        (
            "beta_changed",
            if outcome.beta_changed {
                "true"
            } else {
                "false"
            },
        ),
    ])
}

fn write_channels(args: &Args) -> Result<Outcome> {
    let tag = ReleaseTag::parse(&args.tag)?;
    let mut records = read_records(&args.mirror)?;
    ensure!(
        records.iter().any(|record| record.tag == tag),
        "{} has no record of {tag}; upload the release's record before composing the channels",
        args.mirror.display()
    );
    if let Some(stand_in) = stable_stand_in(&args.mirror, &records)? {
        records.push(stand_in);
    }
    let selection = select(&records)?;
    let files = compose(&selection, &args.base_url)?;
    write_files(&args.output_dir, &files)?;

    let beta = selection.beta_channel();
    let beta_version = beta.tag.version().to_string();
    Ok(Outcome {
        stable_tag: selection.stable.tag.to_string(),
        beta_tag: beta.tag.to_string(),
        beta_changed: served_version(&args.mirror, UpdateChannel::Beta).as_deref()
            != Some(beta_version.as_str()),
        beta_version,
    })
}

/// Every release record in `mirror`, oldest first. A release without a
/// record directory predates records and is skipped; a record missing a file
/// is an error.
fn read_records(mirror: &Path) -> Result<Vec<Record>> {
    let releases = mirror.join(update_feed::RELEASES_DIR);
    if !releases.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs_err::read_dir(&releases)? {
        let entry = entry?;
        if !entry.path().join(update_feed::RECORD_DIR_NAME).is_dir() {
            continue;
        }
        let name = entry.file_name();
        let tag = name
            .to_str()
            .map(ReleaseTag::parse)
            .transpose()?
            .with_context(|| {
                format!(
                    "{} holds a record, but is not named for a release tag",
                    entry.path().display()
                )
            })?;
        records.push(read_record(
            &mirror.join(update_feed::record_dir(&tag)),
            tag,
        )?);
    }
    records.sort_by(|a, b| a.build.cmp(&b.build));
    Ok(records)
}

/// The record of `tag` in `dir`: its manifest, and one appcast per
/// architecture holding the one item.
fn read_record(dir: &Path, tag: ReleaseTag) -> Result<Record> {
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let (manifest, manifest_text) = read_manifest(&manifest_path)?;
    check_manifest(&manifest, &tag, &manifest_path)?;
    let mut build = None;
    let mut items = Vec::new();
    for arch in MacArch::VARIANTS {
        let path = dir.join(arch.appcast_file_name());
        let found = feed_items(&fs_err::read_to_string(&path)?)
            .with_context(|| format!("could not read {}", path.display()))?;
        let [item] = &found[..] else {
            bail!(
                "{} holds {} items; a record holds its release's one",
                path.display(),
                found.len()
            );
        };
        let item_build = item
            .check(&tag)
            .with_context(|| format!("{} is not {tag}'s record", path.display()))?;
        same_build(&mut build, item_build, &path)?;
        items.push(item.text.clone());
    }
    Ok(Record {
        tag,
        build: build.context("no macOS architectures")?,
        manifest,
        manifest_text,
        items,
    })
}

/// The newest stable release's record when that release predates records:
/// the stable channel's current files. `None` when the stable channel has no
/// manifest, or its release has a record. A beta item composed into the
/// stable feed beside the release's is not part of it.
fn stable_stand_in(mirror: &Path, records: &[Record]) -> Result<Option<Record>> {
    let dir = mirror.join(UpdateChannel::Stable.dir());
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let (manifest, manifest_text) = read_manifest(&manifest_path)?;
    let tag = ReleaseTag::parse(&manifest.tag)
        .with_context(|| format!("{} names no release tag", manifest_path.display()))?;
    if records.iter().any(|record| record.tag == tag) {
        return Ok(None);
    }
    ensure!(
        tag.channel() == UpdateChannel::Stable,
        "{} offers {tag}, which is not a stable release",
        manifest_path.display()
    );
    check_manifest(&manifest, &tag, &manifest_path)?;
    let version = tag.version().to_string();
    let mut build = None;
    let mut items = Vec::new();
    for arch in MacArch::VARIANTS {
        let path = dir.join(arch.appcast_file_name());
        let mut found = feed_items(&fs_err::read_to_string(&path)?)
            .with_context(|| format!("could not read {}", path.display()))?
            .into_iter()
            .filter(|item| {
                item.channel.is_none() && item.short_version.as_deref() == Some(&version)
            });
        let item = found
            .next()
            .with_context(|| format!("{} has no item for {tag}", path.display()))?;
        ensure!(
            found.next().is_none(),
            "{} has two items for {tag}",
            path.display()
        );
        let item_build = item.check(&tag).with_context(|| {
            format!("{} does not offer {tag} as a release does", path.display())
        })?;
        same_build(&mut build, item_build, &path)?;
        items.push(item.text);
    }
    Ok(Some(Record {
        tag,
        build: build.context("no macOS architectures")?,
        manifest,
        manifest_text,
        items,
    }))
}

/// Record that the file at `path` offers `build`, which must be the build the
/// other architectures' files offer.
fn same_build(seen: &mut Option<BuildVersion>, build: BuildVersion, path: &Path) -> Result<()> {
    if let Some(previous) = seen.as_ref() {
        ensure!(
            *previous == build,
            "{} offers build {build}, but another architecture's offers {previous}",
            path.display()
        );
    }
    *seen = Some(build);
    Ok(())
}

/// The newest stable release, and the newest beta when it is newer, both in
/// Sparkle's order of build versions. When it is, its version is newer too:
/// a beta's build version starts with the stable release it follows, which
/// is older than the beta.
fn select(records: &[Record]) -> Result<Selection<'_>> {
    let newest = |channel: UpdateChannel| {
        records
            .iter()
            .filter(|record| record.tag.channel() == channel)
            .max_by(|a, b| (&a.build, a.tag.version()).cmp(&(&b.build, b.tag.version())))
    };
    let stable = newest(UpdateChannel::Stable)
        .context("no stable release has a record, and the stable channel offers none")?;
    let newest_beta = newest(UpdateChannel::Beta);
    let beta = newest_beta.filter(|beta| beta.build > stable.build);
    if let Some(skipped) = newest_beta.filter(|_| beta.is_none())
        && skipped.tag.version() > stable.tag.version()
    {
        eprintln!(
            "{} is numbered past {}, but was cut before it (build {} < {}); the beta channel offers {}",
            skipped.tag, stable.tag, skipped.build, stable.build, stable.tag
        );
    }
    Ok(Selection { stable, beta })
}

/// A channel file: its path relative to the root of the update host, and
/// its contents.
#[derive(Debug)]
struct ChannelFile {
    path: String,
    contents: String,
}

fn compose(selection: &Selection<'_>, base_url: &str) -> Result<Vec<ChannelFile>> {
    let mut beta_manifest = selection.beta_channel().manifest.clone();
    UpdateChannel::Beta
        .as_str()
        .clone_into(&mut beta_manifest.channel);
    let mut files = vec![
        ChannelFile {
            path: format!("{}/{MANIFEST_FILE_NAME}", UpdateChannel::Stable.dir()),
            contents: selection.stable.manifest_text.clone(),
        },
        ChannelFile {
            path: format!("{}/{MANIFEST_FILE_NAME}", UpdateChannel::Beta.dir()),
            contents: beta_manifest.render()?,
        },
    ];
    // Newest first, as Sparkle's feeds list them.
    let offered: Vec<&Record> = selection
        .beta
        .into_iter()
        .chain([selection.stable])
        .collect();
    for (index, arch) in MacArch::VARIANTS.iter().enumerate() {
        let items: Vec<&str> = offered
            .iter()
            .map(|record| record.items[index].as_str())
            .collect();
        let contents = feed(base_url, *arch, &items)?;
        check_feed(&contents, &offered)
            .with_context(|| format!("the composed {} feed is wrong", arch.as_str()))?;
        files.push(ChannelFile {
            path: format!("{}/{}", APPCAST_CHANNEL.dir(), arch.appcast_file_name()),
            contents,
        });
    }
    Ok(files)
}

/// The Sparkle feed `arch`'s bundles read, offering `items`.
fn feed(base_url: &str, arch: MacArch, items: &[&str]) -> Result<String> {
    let envelope = Appcast::new("OpenLogi", update_feed::appcast_url(base_url, arch))
        .description(format!(
            "OpenLogi updates ({} channel)",
            APPCAST_CHANNEL.as_str()
        ))
        .render();
    let head = envelope
        .strip_suffix(FEED_END)
        .context("gpui-updater-release no longer ends a feed the way `release channels` expects")?;
    let mut feed = head.to_owned();
    for item in items {
        feed.push_str("    ");
        feed.push_str(item);
        feed.push('\n');
    }
    feed.push_str(FEED_END);
    Ok(feed)
}

/// Check that the composed `feed` parses into the items of `offered`, in
/// order.
fn check_feed(feed: &str, offered: &[&Record]) -> Result<()> {
    let items = feed_items(feed)?;
    ensure!(
        items.len() == offered.len(),
        "it has {} items, not {}",
        items.len(),
        offered.len()
    );
    for (item, record) in items.iter().zip(offered) {
        let build = item.check(&record.tag)?;
        ensure!(
            build == record.build,
            "{}'s item offers build {build}, not {}",
            record.tag,
            record.build
        );
    }
    Ok(())
}

fn write_files(output_dir: &Path, files: &[ChannelFile]) -> Result<()> {
    if output_dir.exists() {
        ensure!(
            fs_err::read_dir(output_dir)?.next().is_none(),
            "{} is not empty; the channel files are uploaded from it with nothing else",
            output_dir.display()
        );
    }
    for file in files {
        let path = output_dir.join(&file.path);
        if let Some(parent) = path.parent() {
            fs_err::create_dir_all(parent)?;
        }
        fs_err::write(&path, &file.contents)?;
        println!("{}", path.display());
    }
    Ok(())
}

/// The version `channel` offers now, from its manifest in `mirror`. `None`
/// when it has none, or the manifest does not say.
fn served_version(mirror: &Path, channel: UpdateChannel) -> Option<String> {
    let path = mirror.join(channel.dir()).join(MANIFEST_FILE_NAME);
    let text = fs_err::read_to_string(path).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    manifest.get("version")?.as_str().map(str::to_owned)
}

fn read_manifest(path: &Path) -> Result<(Manifest, String)> {
    let text = fs_err::read_to_string(path)?;
    let manifest = serde_json::from_str(&text)
        .with_context(|| format!("{} is not an OpenLogi update manifest", path.display()))?;
    Ok((manifest, text))
}

/// Check that the manifest at `path` is `tag`'s, labelled with its channel.
fn check_manifest(manifest: &Manifest, tag: &ReleaseTag, path: &Path) -> Result<()> {
    ensure!(
        manifest.tag == tag.to_string() && manifest.version == tag.version().to_string(),
        "{} is the manifest of {} ({}), not {tag}",
        path.display(),
        manifest.tag,
        manifest.version
    );
    ensure!(
        manifest.channel == tag.channel().as_str(),
        "{} labels {tag} {:?}, not {:?}",
        path.display(),
        manifest.channel,
        tag.channel().as_str()
    );
    Ok(())
}

/// An item of a Sparkle feed.
#[derive(Debug)]
struct FeedItem {
    /// `sparkle:version`: the bundle version Sparkle compares.
    bundle_version: String,
    /// `sparkle:shortVersionString`: the version people read.
    short_version: Option<String>,
    /// `sparkle:channel`; none for an item every channel is offered.
    channel: Option<String>,
    /// The element as the feed spells it, from `<item>` to `</item>`.
    text: String,
}

impl FeedItem {
    /// Check that this is `tag`'s item as `release appcast` writes it, and
    /// return its build version.
    fn check(&self, tag: &ReleaseTag) -> Result<BuildVersion> {
        let version = tag.version().to_string();
        ensure!(
            self.short_version.as_deref() == Some(version.as_str()),
            "the item is version {:?}, not {version}",
            self.short_version
        );
        let channel = (tag.channel() != APPCAST_CHANNEL).then(|| tag.channel().as_str());
        ensure!(
            self.channel.as_deref() == channel,
            "{tag}'s item is on channel {:?}, not {channel:?}",
            self.channel
        );
        let build: BuildVersion = self.bundle_version.parse()?;
        build.ensure_fits(tag)?;
        Ok(build)
    }
}

/// The items of the Sparkle feed `xml`, in order.
fn feed_items(xml: &str) -> Result<Vec<FeedItem>> {
    let doc = roxmltree::Document::parse(xml).context("the feed is not XML")?;
    let channel = doc
        .root_element()
        .children()
        .find(|node| node.has_tag_name("channel"))
        .context("the feed has no <channel>")?;
    channel
        .children()
        .filter(|node| node.has_tag_name("item"))
        .map(|node| {
            let sparkle = |name: &str| {
                node.children()
                    .find(|child| child.has_tag_name((SPARKLE_NAMESPACE, name)))
                    .and_then(|child| child.text())
                    .map(str::to_owned)
            };
            Ok(FeedItem {
                bundle_version: sparkle("version").context("an item has no sparkle:version")?,
                short_version: sparkle("shortVersionString"),
                channel: sparkle("channel"),
                text: xml[node.range()].to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
