mod macos;
mod manifest;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use macos::DmgMacos;
use manifest::GenerateUpdaterManifest;

#[derive(Parser)]
#[command(about = "OpenLogi repository maintenance tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate the static updater manifest consumed by gpui-updater.
    GenerateUpdaterManifest(GenerateUpdaterManifest),
    /// Generate the macOS app icon from the master SVG.
    MacosIcns,
    /// Build the release OpenLogi.app bundle.
    BundleMacos,
    /// Create the branded macOS DMG from an existing app bundle.
    DmgMacos(DmgMacos),
    /// Build the app bundle and package it into the branded macOS DMG.
    PackageMacos(DmgMacos),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::GenerateUpdaterManifest(args) => manifest::generate_updater_manifest(&args),
        Command::MacosIcns => macos::generate_macos_icns(),
        Command::BundleMacos => macos::bundle_macos(),
        Command::DmgMacos(args) => macos::dmg_macos(&args),
        Command::PackageMacos(args) => macos::package_macos(&args),
    }
}
