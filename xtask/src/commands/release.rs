pub(crate) mod latest_json;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Generate the static latest.json updater manifest consumed by gpui-updater.
    LatestJson(latest_json::Args),
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::LatestJson(args) => latest_json::run(&args),
    }
}
