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
        Command::Macos(command) => commands::macos::run(command),
        Command::Linux(command) => commands::linux::run(command),
        Command::Release(command) => commands::release::run(command),
    }
}
