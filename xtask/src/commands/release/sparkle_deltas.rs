//! Sparkle deltas from the newest earlier releases to this one.
//!
//! Runs on the macOS build leg right after the DMG is stapled: Sparkle's
//! `BinaryDelta` is a macOS tool, and the publish job is not. Each delta is
//! applied to its old app and compared with the new one before it is kept
//! (`gpui-updater-release` removes one that fails), and the metadata the
//! publish job's `release appcast` needs is written next to it.
//!
//! A delta that cannot be made is a warning, not a failure. The feed still
//! offers the full DMG, which every installed app can take, so a missing delta
//! costs one larger download and never an update. This holds for the shared
//! setup too (listing releases, fetching Sparkle, reading the new DMG): only
//! arguments that cannot name a release DMG fail the command.
//!
//! Only stable releases get deltas, made from earlier stable releases. A beta
//! gets none, so a beta update always downloads the full DMG.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;
use gpui_updater_release::sparkle::{self, DeltaOptions, SparkleDistribution};
use openlogi_core::update_feed::UpdateChannel;
use semver::Version;
use xshell::{Shell, cmd};

use crate::commands::macos::bundle::sparkle as bundled_sparkle;
use crate::support::fs::repo_root;
use crate::support::release_tag::ReleaseTag;
use crate::support::update_feed::MacArch;

/// The first release whose bundle embeds Sparkle. Older apps update through
/// `latest.json` and never read an appcast, so a delta from them is never
/// used.
const FIRST_SPARKLE_RELEASE: Version = Version::new(0, 8, 14);

#[derive(Parser)]
pub(crate) struct Args {
    /// The stapled `OpenLogi-<tag>-macos-<arch>.dmg`. The deltas and their
    /// metadata are written next to it.
    #[arg(long)]
    dmg: PathBuf,
    /// Release tag of the DMG, for example `v0.8.16`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// How many of the newest earlier releases get a delta. Sparkle offers a
    /// delta only to an app at exactly its base version, so older apps take
    /// the full DMG.
    #[arg(long, default_value_t = 3)]
    count: usize,
    /// Repository whose GitHub Releases hold the earlier DMGs.
    #[arg(long, env = "GITHUB_REPOSITORY", default_value = "AprilNEA/OpenLogi")]
    repo: String,
}

/// The release tags to make deltas from: final releases that embed Sparkle
/// and are older than `current`, newest first, at most `count`.
fn delta_bases<'a>(tags: &'a [String], current: &Version, count: usize) -> Vec<&'a str> {
    let mut bases: Vec<(Version, &str)> = tags
        .iter()
        .filter_map(|tag| Some((ReleaseTag::stable_version(tag)?, tag.as_str())))
        .filter(|(version, _)| *version >= FIRST_SPARKLE_RELEASE && version < current)
        .collect();
    bases.sort_unstable_by(|a, b| b.cmp(a));
    bases.dedup_by(|a, b| a.0 == b.0);
    bases.into_iter().take(count).map(|(_, tag)| tag).collect()
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let tag = ReleaseTag::parse(&args.tag)?;
    if tag.channel() != UpdateChannel::Stable {
        println!(
            "::notice::no Sparkle deltas for {tag}: only stable releases get them, so a {} update downloads the full DMG",
            tag.channel().as_str()
        );
        return Ok(());
    }
    let current = tag.version().clone();
    let dmg_name = args
        .dmg
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 file name", args.dmg.display()))?;
    let arch = MacArch::of_dmg(dmg_name)
        .with_context(|| format!("{dmg_name} is not an `…-macos-<arch>.dmg` release DMG"))?;
    let out_dir = args
        .dmg
        .parent()
        .context("the DMG has no parent directory")?;
    if let Err(error) = make_deltas(args, &current, arch, dmg_name, out_dir) {
        println!("::warning::no Sparkle deltas for {dmg_name}: {error:#}");
    }
    Ok(())
}

/// Make every delta for one DMG. An error here is from the shared setup;
/// [`run`] reports it as a warning, and a failed base is reported here.
fn make_deltas(
    args: &Args,
    current: &Version,
    arch: MacArch,
    dmg_name: &str,
    out_dir: &Path,
) -> Result<()> {
    let sh = Shell::new()?;
    let repo = &args.repo;
    let listed = cmd!(
        sh,
        "gh release list --repo {repo} --exclude-drafts --exclude-pre-releases --limit 100 --json tagName --jq .[].tagName"
    )
    .read()
    .context("could not list the earlier releases")?;
    let tags: Vec<String> = listed.lines().map(str::to_owned).collect();
    let bases = delta_bases(&tags, current, args.count);
    if bases.is_empty() {
        println!("no earlier Sparkle release to make a delta from");
        return Ok(());
    }

    let dist = SparkleDistribution::from_dir(&bundled_sparkle::fetch_release(&repo_root()?)?)?;
    let scratch = tempfile::tempdir().context("could not create a scratch directory")?;
    let new_app = sparkle::extract_app(&args.dmg, &scratch.path().join("new"), None)
        .with_context(|| format!("could not copy the app out of {}", args.dmg.display()))?;
    let maker = DeltaMaker {
        sh: &sh,
        repo,
        arch,
        dist: &dist,
        new_app: &new_app,
        dmg_name,
        out_dir,
        scratch: scratch.path(),
    };
    let mut made = 0;
    for base in &bases {
        match maker.make(base) {
            Ok(delta) => {
                made += 1;
                println!("{}", delta.display());
            }
            Err(error) => println!("::warning::no Sparkle delta from {base}: {error:#}"),
        }
    }
    println!("{made} of {} Sparkle deltas made", bases.len());
    Ok(())
}

/// What every delta of one DMG shares.
struct DeltaMaker<'a> {
    sh: &'a Shell,
    repo: &'a str,
    arch: MacArch,
    dist: &'a SparkleDistribution,
    new_app: &'a Path,
    dmg_name: &'a str,
    out_dir: &'a Path,
    scratch: &'a Path,
}

impl DeltaMaker<'_> {
    /// Download `base`'s DMG of this architecture, make the verified delta
    /// from its app, and write the metadata next to it.
    fn make(&self, base: &str) -> Result<PathBuf> {
        let dir = self.scratch.join(base);
        fs_err::create_dir_all(&dir)?;
        let asset = format!("OpenLogi-{base}-macos-{}.dmg", self.arch.as_str());
        let (sh, repo) = (self.sh, self.repo);
        cmd!(
            sh,
            "gh release download {base} --repo {repo} --pattern {asset} --dir {dir}"
        )
        .run()
        .with_context(|| format!("could not download {asset}"))?;
        let old_dmg = dir.join(&asset);
        let old_app = sparkle::extract_app(&old_dmg, &dir.join("app"), None)
            .with_context(|| format!("could not copy the app out of {asset}"))?;
        // The DMG is no longer needed; the runner's disk is not unlimited.
        fs_err::remove_file(&old_dmg)?;
        let from = sparkle::delta::inspect(&old_app)?.bundle_version;
        let out = self
            .out_dir
            .join(sparkle::delta::delta_file_name(self.dmg_name, &from));
        let delta = sparkle::delta::create(
            self.dist,
            &old_app,
            self.new_app,
            &out,
            &DeltaOptions::default(),
        )?;
        delta.write_metadata()?;
        fs_err::remove_dir_all(&dir)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
