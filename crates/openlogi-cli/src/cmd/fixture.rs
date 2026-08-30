//! Privacy-safe mock-device fixture commands.

use anyhow::Result;
use clap::Subcommand;

pub(crate) mod record_case;

/// Commands that create or inspect mock-device fixtures.
#[derive(Debug, Subcommand)]
pub enum FixtureCmd {
    /// Record fixture data from direct, read-only hardware access.
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
}

impl FixtureRecordCmd {
    async fn run(self) -> Result<()> {
        match self {
            Self::Case(args) => record_case::run(args).await,
        }
    }
}
