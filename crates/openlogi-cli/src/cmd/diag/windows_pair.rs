//! `openlogi diag windows-pair` - exercise Windows Bluetooth pairing.

use anyhow::{Result, bail};
use clap::Args;
use openlogi_hid::{WindowsPairingDevice, pair_windows_device};

#[derive(Debug, Args)]
pub struct WindowsPairArgs {
    /// Only list Windows Bluetooth pairing candidates.
    #[arg(long)]
    pub list: bool,

    /// Pair this Windows device id.
    #[arg(long)]
    pub id: Option<String>,

    /// Pair the first listed candidate.
    #[arg(long)]
    pub first: bool,
}

pub async fn run(args: WindowsPairArgs) -> Result<()> {
    let devices = openlogi_hid::list_windows_pairing_devices().await?;
    if devices.is_empty() {
        if args.list {
            println!("no Windows Bluetooth pairing candidates found");
            return Ok(());
        }
        bail!("no Windows Bluetooth pairing candidates found");
    }

    println!("Windows Bluetooth pairing candidates:");
    for (idx, device) in devices.iter().enumerate() {
        println!("  {}. {}", idx + 1, format_device(device));
        println!("      id={}", device.id);
    }

    if args.list {
        return Ok(());
    }

    let device = select_device(&devices, args.id.as_deref(), args.first)?;
    println!("pairing through Windows Bluetooth: {}", device.name);
    let outcome = pair_windows_device(device.id.clone()).await?;
    println!(
        "Windows pairing result: {} ({})",
        outcome.device.name, outcome.status
    );
    if outcome.succeeded() {
        Ok(())
    } else {
        bail!("Windows pairing did not complete: {}", outcome.status)
    }
}

fn select_device<'a>(
    devices: &'a [WindowsPairingDevice],
    id: Option<&str>,
    first: bool,
) -> Result<&'a WindowsPairingDevice> {
    if let Some(id) = id {
        return devices
            .iter()
            .find(|device| device.id == id)
            .ok_or_else(|| anyhow::anyhow!("Windows device id not found: {id}"));
    }
    if first || devices.len() == 1 {
        return Ok(&devices[0]);
    }
    bail!("multiple candidates found; pass --id or --first")
}

fn format_device(device: &WindowsPairingDevice) -> String {
    let logitech = if device.likely_logitech {
        " likely-logitech"
    } else {
        ""
    };
    format!(
        "{} can_pair={} paired={}{}",
        device.name, device.can_pair, device.is_paired, logitech
    )
}
