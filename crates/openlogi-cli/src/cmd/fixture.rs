//! Privacy-safe mock-device fixture commands.

use anyhow::Result;
use clap::Subcommand;

pub(crate) mod contribute;
mod output;
pub(crate) mod record_case;
pub(crate) mod record_profile;
mod target_selection;
pub(crate) mod verify;

/// Commands that create or inspect mock-device fixtures.
#[derive(Debug, Subcommand)]
pub enum FixtureCmd {
    /// Build a complete privacy-safe fixture with a resumable two-phase wizard.
    ///
    /// Direct capture discovery may enable wireless notifications and request arrival
    /// reports on connected receivers before target selection. Notification flags are
    /// not restored; captured operations do not change device settings or pairings.
    /// Use --profile-only to avoid direct hardware access.
    Contribute(contribute::ContributeArgs),
    /// Capture privacy-safe semantic state or named hardware reads.
    #[command(subcommand)]
    Record(FixtureRecordCmd),
    /// Strictly verify one complete on-disk fixture directory.
    Verify(verify::VerifyArgs),
}

impl FixtureCmd {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Contribute(args) => contribute::run(args).await,
            Self::Record(command) => command.run().await,
            Self::Verify(args) => verify::run(&args),
        }
    }
}

/// Fixture recording commands with read-only captured operations.
#[derive(Debug, Subcommand)]
pub enum FixtureRecordCmd {
    /// Record one named production read as a strict HID cassette.
    ///
    /// Discovery may enable wireless notifications and request arrival reports on
    /// connected receivers before target selection. Notification flags are not
    /// restored; captured operations do not change device settings or pairings.
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
