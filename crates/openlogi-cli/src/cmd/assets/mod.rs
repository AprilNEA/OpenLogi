//! `openlogi assets <subcommand>` — bulk asset-host operations.

use anyhow::Result;
use clap::Subcommand;

pub mod sync;

#[derive(Debug, Subcommand)]
pub enum AssetsCmd {
    /// Download every device's bundle-required files from the fastest available mirror.
    Sync(sync::SyncArgs),
}

impl AssetsCmd {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Sync(args) => sync::run(args),
        }
    }
}
