pub(crate) mod bundle;
pub(crate) mod dmg;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Generate the macOS app icon from the master PNG.
    Icns,
    /// Build the release OpenLogi.app bundle.
    Bundle,
    /// Create the branded macOS DMG from an existing app bundle.
    Dmg(dmg::Args),
    /// Build the app bundle, optionally sign it, and package the branded DMG.
    Package(dmg::Args),
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Icns => bundle::generate_icns(),
        Command::Bundle => bundle::run(),
        Command::Dmg(args) => dmg::run(&args),
        Command::Package(args) => {
            bundle::run()?;
            if let Some(identity) = &args.sign_identity {
                bundle::sign_app(identity)?;
            } else {
                println!("==> codesign: skipped (unsigned — set OPENLOGI_SIGN_IDENTITY to sign)");
            }
            dmg::run(&args)
        }
    }
}
