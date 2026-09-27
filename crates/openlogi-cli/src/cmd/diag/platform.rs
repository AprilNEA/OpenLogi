//! `openlogi diag platform` — inspect or set the `0x4531` MultiPlatform mode.
//!
//! Multi-OS keyboards (ERGO K860, MX Keys, …) switch their key behavior per
//! host between platforms such as macOS and Windows. This prints the platform
//! descriptors and each host slot's selected platform, so the current mode can
//! be read without pressing a key, and `--set` selects one for the current
//! host.

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use openlogi_hid::{HostOperatingSystem, HostPlatformApply};

use crate::cmd::diag::select_device;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PlatformArg {
    /// Windows (PC) key behavior.
    Windows,
    /// macOS key behavior.
    Macos,
    /// Linux key behavior, where the keyboard advertises it.
    Linux,
}

impl From<PlatformArg> for HostOperatingSystem {
    fn from(value: PlatformArg) -> Self {
        match value {
            PlatformArg::Windows => Self::Windows,
            PlatformArg::Macos => Self::MacOs,
            PlatformArg::Linux => Self::Linux,
        }
    }
}

#[derive(Debug, Args)]
pub struct PlatformArgs {
    /// Platform to select for the keyboard's current host slot, written
    /// directly to the device and verified by readback.
    #[arg(long, value_enum)]
    pub set: Option<PlatformArg>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: PlatformArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x4531]).await?;
    println!("device: {name} ({route})");

    if let Some(platform) = args.set {
        let applied = openlogi_hid::set_native_host_platform(&route, platform.into())
            .await
            .context("set MultiPlatform host platform")?;
        match applied {
            HostPlatformApply::AlreadySelected { platform_index } => {
                println!("  {platform:?} (platform {platform_index}) was already selected");
            }
            HostPlatformApply::Updated { platform_index } => {
                println!("  selected {platform:?} (platform {platform_index}), verified");
            }
            _ => println!("  {applied:?}"),
        }
    }

    let report = openlogi_hid::read_platform_raw(&route)
        .await
        .context("read MultiPlatform state")?;
    for line in report.lines() {
        println!("  {line}");
    }
    Ok(())
}
