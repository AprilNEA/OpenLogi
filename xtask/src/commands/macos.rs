pub(crate) mod bundle;
pub(crate) mod dmg;

use anyhow::Result;
use clap::{Parser, Subcommand};

use bundle::identity::Channel;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Generate the macOS app icon from the master PNG.
    Icns,
    /// Build the OpenLogi.app bundle.
    Bundle(BundleArgs),
    /// Create the branded macOS DMG from an existing app bundle.
    Dmg(dmg::Args),
    /// Build the app bundle for distribution, optionally sign it, and package
    /// the branded DMG.
    Package(dmg::Args),
}

#[derive(Parser)]
pub(crate) struct BundleArgs {
    /// Identity family to stamp into the bundle. Defaults to `dev` so a local
    /// build can never claim the installed app's permission grants or config;
    /// the shipped bundle comes from `macos package`.
    #[arg(long, value_enum, default_value_t = Channel::Dev)]
    channel: Channel,
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Icns => bundle::generate_icns(),
        Command::Bundle(args) => bundle::run(args.channel),
        Command::Dmg(args) => dmg::run(&args),
        Command::Package(args) => {
            bundle::run_for_distribution(args.sign_identity.as_deref())?;
            dmg::run(&args)
        }
    }
}
