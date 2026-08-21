mod commands;
mod support;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "OpenLogi repository maintenance tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Reproduce the CI jobs this host can run.
    Ci(commands::ci::Args),
    /// macOS app bundle, icon, and DMG tasks.
    #[command(subcommand)]
    Macos(commands::macos::Command),
    /// Linux package tasks.
    #[command(subcommand)]
    Linux(commands::linux::Command),
    /// Release metadata tasks.
    #[command(subcommand)]
    Release(commands::release::Command),
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Ci(args) => commands::ci::run(&args),
        Command::Macos(command) => commands::macos::run(command),
        Command::Linux(command) => commands::linux::run(command),
        Command::Release(command) => commands::release::run(command),
    }
}
