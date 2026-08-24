pub(crate) mod changelog;
pub(crate) mod check_publish;
pub(crate) mod checkout_version_bump;
pub(crate) mod latest_json;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Write the next workspace version's section into CHANGELOG.md with git-cliff.
    Changelog(changelog::Args),
    /// Verify that crates.io packages have a publishable workspace dependency closure.
    CheckPublish,
    /// Pin the release job to the commit that introduced the workspace version.
    CheckoutVersionBump,
    /// Generate the static latest.json updater manifest consumed by gpui-updater.
    LatestJson(latest_json::Args),
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Changelog(args) => changelog::run(&args),
        Command::CheckPublish => check_publish::run(),
        Command::CheckoutVersionBump => checkout_version_bump::run(),
        Command::LatestJson(args) => latest_json::run(&args),
    }
}
