//! `openlogi diag` — real-device smoke tests for the HID++ write path.
//!
//! Each subcommand exercises one round-trip (read → modify → read back →
//! restore). The intent is verification, not configuration: nothing here
//! touches `config.toml` or talks to the GUI; everything runs through the
//! same `openlogi_hid` API the GPUI app uses, so a green diag means the
//! GUI's write path works on this host.

use anyhow::Result;
use clap::Subcommand;
use openlogi_hid::{DeviceRoute, DirectDeviceIdentity};

pub mod dpi;
pub mod features;
pub mod smartshift;

#[derive(Debug, Subcommand)]
pub enum DiagCmd {
    /// Dump every HID++ feature the active device reports.
    Features(features::FeaturesArgs),
    /// Read DPI → write a small delta → read back → restore → report.
    Dpi(dpi::DpiArgs),
    /// Read SmartShift mode → toggle → read back → toggle back → report.
    Smartshift(smartshift::SmartshiftArgs),
}

impl DiagCmd {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Features(args) => features::run(args).await,
            Self::Dpi(args) => dpi::run(args).await,
            Self::Smartshift(args) => smartshift::run(args).await,
        }
    }
}

/// Shared device picker: enumerate inventories, return the [`DeviceRoute`] +
/// display name of the first online paired device (the same selection rule the
/// GUI uses for its initial target). Builds a Bolt route when the device is
/// behind a receiver, a direct route otherwise (USB cable / Bluetooth).
pub(crate) async fn first_online_device() -> Result<(DeviceRoute, String)> {
    use anyhow::anyhow;
    let inventories = openlogi_hid::enumerate().await?;
    inventories
        .into_iter()
        .find_map(|inv| {
            let paired = inv.paired.into_iter().find(|p| p.online)?;
            let route = if let Some(receiver_uid) = inv.receiver.unique_id {
                DeviceRoute::Bolt {
                    receiver_uid,
                    slot: paired.slot,
                }
            } else {
                let model = paired.model_info.as_ref()?;
                DeviceRoute::Direct {
                    vendor_id: inv.receiver.vendor_id,
                    product_id: inv.receiver.product_id,
                    identity: DirectDeviceIdentity::from_model(model),
                }
            };
            let name = paired
                .codename
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            Some((route, name))
        })
        .ok_or_else(|| anyhow!("no online HID++ device found — is a Logi mouse paired?"))
}
