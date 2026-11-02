pub(crate) mod appcast;
pub(crate) mod changelog;
pub(crate) mod channels;
pub(crate) mod check_publish;
pub(crate) mod checkout_version_bump;
pub(crate) mod latest_json;
pub(crate) mod next_beta;
pub(crate) mod sparkle_deltas;
pub(crate) mod stamp_version;
pub(crate) mod version;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Write the release's EdDSA-signed Sparkle feed record for each macOS architecture.
    Appcast(appcast::Args),
    /// Write the next workspace version's section into CHANGELOG.md with git-cliff.
    Changelog(changelog::Args),
    /// Compose every channel's update feeds from the release records on the update host.
    Channels(channels::Args),
    /// Verify that crates.io packages have a publishable workspace dependency closure.
    CheckPublish,
    /// Pin the release job to the commit that introduced the workspace version.
    CheckoutVersionBump,
    /// Write the release's static latest.json updater manifest, consumed by gpui-updater.
    LatestJson(latest_json::Args),
    /// Name the beta tag to cut on HEAD, checked as `version` checks the
    /// pushed tag.
    NextBeta(next_beta::Args),
    /// Make verified Sparkle deltas from the newest earlier releases to a stapled DMG.
    SparkleDeltas(sparkle_deltas::Args),
    /// Give the workspace a release's version before it builds: a beta's own
    /// version over the stable release its tagged commit carries.
    StampVersion(stamp_version::Args),
    /// Print a release tag's channel, version, build version and the version
    /// its build stamps, checked against the tagged commit.
    Version(version::Args),
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Appcast(args) => appcast::run(&args),
        Command::Changelog(args) => changelog::run(&args),
        Command::Channels(args) => channels::run(&args),
        Command::CheckPublish => check_publish::run(),
        Command::CheckoutVersionBump => checkout_version_bump::run(),
        Command::LatestJson(args) => latest_json::run(&args),
        Command::NextBeta(args) => next_beta::run(&args),
        Command::SparkleDeltas(args) => sparkle_deltas::run(&args),
        Command::StampVersion(args) => stamp_version::run(&args),
        Command::Version(args) => version::run(&args),
    }
}
