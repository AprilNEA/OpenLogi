//! `openlogi diag` — real-device smoke tests for the HID++ write path.
//!
//! Each subcommand exercises one round-trip (read → modify → read back →
//! restore). The intent is verification, not configuration: nothing here
//! touches `config.toml` or talks to the GUI; everything runs through the
//! same `openlogi_hid` API the GPUI app uses, so a green diag means the
//! GUI's write path works on this host.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use openlogi_hid::DeviceRoute;
use tokio::time::{Duration, sleep};

pub mod dpi;
pub mod features;
pub mod lighting;
pub mod pair;
pub mod smartshift;
pub mod windows_pair;

#[derive(Debug, Subcommand)]
pub enum DiagCmd {
    /// Dump every HID++ feature the active device reports.
    Features(features::FeaturesArgs),
    /// Read DPI → write a small delta → read back → restore → report.
    Dpi(dpi::DpiArgs),
    /// Run a receiver pairing session and print every pairing event.
    Pair(pair::PairArgs),
    /// Pair through the Windows Bluetooth pairing API.
    WindowsPair(windows_pair::WindowsPairArgs),
    /// Read SmartShift mode → toggle → read back → toggle back → report.
    Smartshift(smartshift::SmartshiftArgs),
    /// Set a wired RGB keyboard to a solid colour (e.g. `ff0000` for red).
    Lighting(lighting::LightingArgs),
}

impl DiagCmd {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Features(args) => features::run(args).await,
            Self::Dpi(args) => dpi::run(args).await,
            Self::Pair(args) => pair::run(args).await,
            Self::WindowsPair(args) => windows_pair::run(args).await,
            Self::Smartshift(args) => smartshift::run(args).await,
            Self::Lighting(args) => lighting::run(args).await,
        }
    }
}

/// Shared device picker: enumerate inventories, return the [`DeviceRoute`] +
/// display name of the first online paired device (the same selection rule the
/// GUI uses for its initial target). Builds a Bolt route when the device is
/// behind a receiver, a direct route otherwise (USB cable / Bluetooth).
pub(crate) async fn first_online_device() -> Result<(DeviceRoute, String)> {
    const ATTEMPTS: usize = 3;

    for attempt in 0..ATTEMPTS {
        let inventories = openlogi_hid::enumerate().await?;
        if let Some(device) = pick_online_device(inventories) {
            return Ok(device);
        }
        if attempt + 1 < ATTEMPTS {
            sleep(Duration::from_millis(600)).await;
        }
    }

    Err(anyhow!(
        "no online HID++ device found — is a Logi mouse paired?"
    ))
}

fn pick_online_device(
    inventories: Vec<openlogi_core::device::DeviceInventory>,
) -> Option<(DeviceRoute, String)> {
    inventories.into_iter().find_map(|inv| {
        let paired = inv
            .paired
            .into_iter()
            .find(|p| p.online && p.model_info.is_some())?;
        let route = match inv.receiver.unique_id {
            Some(receiver_uid) => DeviceRoute::Bolt {
                receiver_uid,
                slot: paired.slot,
            },
            None => DeviceRoute::Direct {
                vendor_id: inv.receiver.vendor_id,
                product_id: inv.receiver.product_id,
            },
        };
        let name = paired
            .codename
            .unwrap_or_else(|| format!("Slot {}", paired.slot));
        Some((route, name))
    })
}
