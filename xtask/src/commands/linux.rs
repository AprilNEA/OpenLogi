pub(crate) mod appimage;
mod arch;
pub(crate) mod package;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Build release binaries and package them into .deb, .rpm, and .pkg.tar.zst.
    Package(package::Args),
    /// Build release binaries and bundle them into a self-contained .AppImage.
    Appimage(appimage::Args),
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Package(args) => package::run(&args),
        Command::Appimage(args) => appimage::run(&args),
    }
}
