use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

pub mod assets;
pub mod backlight;
pub mod camera;
pub mod diag;
pub mod light;
pub mod list;
pub mod snapshot;
pub mod unpair;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List connected Logitech HID++ devices.
    List(list::ListArgs),
    /// Read or persistently set the keyboard backlight (HID++ 0x1982).
    Backlight(backlight::BacklightArgs),
    /// Capture one frame from a Logitech webcam to a PNG.
    Snapshot(snapshot::SnapshotArgs),
    /// Read or write device-level UVC image controls on a webcam.
    Camera(camera::CameraArgs),
    /// Manage assets fetched from OpenLogi's asset mirrors.
    #[command(subcommand)]
    Assets(assets::AssetsCmd),
    /// Real-device round-trip smoke tests against the HID++ write path.
    #[command(subcommand)]
    Diag(diag::DiagCmd),
    /// Inspect and control standalone Logitech lights.
    #[command(subcommand)]
    Light(light::LightCmd),
    /// Remove a device from a receiver's pairing table.
    Unpair(unpair::UnpairArgs),
}

impl Command {
    /// Dispatch the parsed subcommand and report the process exit status.
    ///
    /// Only `list` reports a status of its own (nothing connected); every
    /// other subcommand either succeeds or fails outright.
    pub async fn run(self) -> Result<ExitCode> {
        match self {
            Self::List(args) => return list::run(args).await,
            Self::Backlight(args) => backlight::run(args).await?,
            // Camera capture is blocking AVFoundation — no need for the async runtime.
            Self::Snapshot(args) => snapshot::run(args)?,
            // UVC control transfers are blocking IOKit — no async runtime needed.
            Self::Camera(args) => camera::run(args)?,
            // `assets sync` is blocking HTTP — no need for the async runtime.
            Self::Assets(cmd) => cmd.run()?,
            Self::Diag(cmd) => cmd.run().await?,
            Self::Light(cmd) => cmd.run().await?,
            Self::Unpair(args) => unpair::run(args).await?,
        }
        Ok(ExitCode::SUCCESS)
    }
}
