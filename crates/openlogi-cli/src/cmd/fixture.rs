//! Privacy-safe mock-device fixture commands.

use anyhow::Result;
use clap::Subcommand;

mod output;
pub(crate) mod record_case;
pub(crate) mod record_profile;

/// Commands that create or inspect mock-device fixtures.
#[derive(Debug, Subcommand)]
pub enum FixtureCmd {
    /// Capture privacy-safe fixture data through a read-only owner.
    #[command(subcommand)]
    Record(FixtureRecordCmd),
}

impl FixtureCmd {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Record(command) => command.run().await,
        }
    }
}

/// Read-only fixture recording commands.
#[derive(Debug, Subcommand)]
pub enum FixtureRecordCmd {
    /// Record one named production read as a strict HID cassette.
    Case(record_case::RecordCaseArgs),
    /// Capture semantic state through the running Agent IPC, without direct hardware access.
    Profile(record_profile::RecordProfileArgs),
}

impl FixtureRecordCmd {
    async fn run(self) -> Result<()> {
        match self {
            Self::Case(args) => record_case::run(args).await,
            Self::Profile(args) => record_profile::run(args).await,
        }
    }
}
