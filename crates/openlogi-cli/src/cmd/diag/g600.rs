//! `openlogi diag g600` — read a G600's onboard state over its feature
//! reports.
//!
//! The G600 speaks no HID++, so none of the other diags reach it. `status`
//! shows what the GUI's DPI panel is built from; `dump` prints every report
//! as hex for a bug report or a driver fixture.

use anyhow::{Context, Result, anyhow};
use clap::{Args, Subcommand};
use openlogi_hid::{DeviceRoute, G600_DRIVER_ID};

use super::hidpp::hex;

#[derive(Debug, Args)]
pub struct G600Args {
    #[command(subcommand)]
    cmd: G600Cmd,
}

#[derive(Debug, Subcommand)]
enum G600Cmd {
    /// Print the active profile and DPI slot, and each profile's DPI table,
    /// report rate and LED.
    Status,
    /// Print every report as hex pairs, one report per line.
    Dump,
}

pub async fn run(args: G600Args) -> Result<()> {
    let (route, name) = g600_device().await?;
    println!("device: {name} ({route})");
    match args.cmd {
        G600Cmd::Status => status(&route).await,
        G600Cmd::Dump => dump(&route).await,
    }
}

/// The one connected G600, by its vendor collection.
async fn g600_device() -> Result<(DeviceRoute, String)> {
    let devices = openlogi_hid::enumerate_standalone()
        .await
        .context("failed to enumerate standalone HID devices")?;
    let mut mice = devices
        .into_iter()
        .filter(|device| device.driver_id == G600_DRIVER_ID);
    match (mice.next(), mice.next()) {
        (None, _) => Err(anyhow!(
            "no G600 is connected (its 0xff80 vendor collection was not found)"
        )),
        (Some(mouse), None) => Ok((mouse.route(), mouse.display_name)),
        (Some(_), Some(_)) => Err(anyhow!(
            "several G600s are connected; this diag addresses one"
        )),
    }
}

async fn status(route: &DeviceRoute) -> Result<()> {
    let status = openlogi_hid::read_g600_status(route)
        .await
        .context("read the G600's reports")?;
    println!(
        "active: profile {} / dpi slot {}",
        status.active_profile, status.active_dpi_slot
    );
    for (index, profile) in status.profiles.iter().enumerate() {
        let slots: Vec<String> = profile
            .dpi_slots
            .iter()
            .map(|slot| slot.map_or_else(|| "off".to_owned(), |dpi| dpi.to_string()))
            .collect();
        let default = profile
            .default_dpi_slot
            .map_or_else(|| "none".to_owned(), |slot| slot.to_string());
        let shift = profile
            .dpi_shift
            .map_or_else(|| "off".to_owned(), |dpi| dpi.to_string());
        let [r, g, b] = profile.led;
        println!(
            "  profile {index}: dpi {}  default slot {default}  shift {shift}  {} Hz  led #{r:02x}{g:02x}{b:02x}",
            slots.join(" / "),
            profile.report_rate_hz,
        );
    }
    Ok(())
}

async fn dump(route: &DeviceRoute) -> Result<()> {
    let dump = openlogi_hid::dump_g600_reports(route)
        .await
        .context("read the G600's reports")?;
    println!("{}", hex(&dump.active));
    for profile in &dump.profiles {
        println!("{}", hex(profile));
    }
    Ok(())
}
