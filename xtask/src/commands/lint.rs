//! Lint hygiene that one clippy run cannot check on its own.
//!
//! `clippy::allow_attributes` makes every `#[allow]` justify itself, but an
//! `allow` that stops suppressing anything stays silent forever. The only way
//! to tell is to remove it and see whether its lint fires — on every host,
//! because the five `allow`s left in this tree each fire on only some hosts or
//! builds. `sweep-allows` does that on the current host; `merge-allows` joins
//! the per-host reports and fails on an `allow` that no host needs.

mod allow_sites;
mod report;
mod sweep;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Remove each `allow` in turn and record which of its lints fire here.
    SweepAllows(SweepArgs),
    /// Join per-host sweep reports; fail on an `allow` no host needs.
    MergeAllows(MergeArgs),
}

#[derive(Args)]
pub(crate) struct SweepArgs {
    /// Where to write this host's report (JSON).
    #[arg(long, default_value = "target/stale-allows.json")]
    out: PathBuf,
}

#[derive(Args)]
pub(crate) struct MergeArgs {
    /// Sweep reports, one per host.
    #[arg(required = true)]
    reports: Vec<PathBuf>,
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::SweepAllows(args) => sweep::run(&args.out),
        Command::MergeAllows(args) => report::run_merge(&args.reports),
    }
}
