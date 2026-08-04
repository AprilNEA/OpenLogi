use anyhow::Result;
use clap::Subcommand;

pub mod assets;
pub mod backlight;
pub mod diag;
pub mod list;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List connected Logitech HID++ devices.
    List(list::ListArgs),
    /// Read or persistently set the keyboard backlight (HID++ 0x1982).
    Backlight(backlight::BacklightArgs),
    /// Manage assets fetched from OpenLogi's asset mirrors.
    #[command(subcommand)]
    Assets(assets::AssetsCmd),
    /// Real-device round-trip smoke tests against the HID++ write path.
    #[command(subcommand)]
    Diag(diag::DiagCmd),
}

impl Command {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::List(args) => list::run(args).await,
            Self::Backlight(args) => backlight::run(args).await,
            // `assets sync` is blocking HTTP — no need for the async runtime.
            Self::Assets(cmd) => cmd.run(),
            Self::Diag(cmd) => cmd.run().await,
        }
    }
}
