pub(crate) mod package;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Build release binaries and package them into .deb and .rpm.
    Package(package::Args),
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Package(args) => package::run(&args),
    }
}
