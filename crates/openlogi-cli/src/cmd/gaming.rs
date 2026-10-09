//! Direct, explicit gaming-memory inspection and backup.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use openlogi_device::{DeviceRoute, gaming};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

#[derive(Debug, Args)]
pub struct GamingArgs {
    /// Exact direct USB product ID in hexadecimal (G502 X LIGHTSPEED wired: c098).
    #[arg(long, default_value = "c098", value_parser = parse_product_id)]
    product_id: u16,
    #[command(subcommand)]
    command: GamingCommand,
}

#[derive(Debug, Subcommand)]
enum GamingCommand {
    /// Read onboard profile fields, including normal/G-Shift bindings. No writes.
    Inspect {
        /// Inspect a saved capture instead of opening the mouse.
        #[arg(long)]
        input: Option<PathBuf>,
    },
    /// Prepare and validate an edit entirely offline.
    Prepare {
        original: PathBuf,
        patch: PathBuf,
        sector: u16,
        output: PathBuf,
    },
    /// Show the before/after fields of a prepared edit; never touches hardware.
    Preview { edit: PathBuf },
    /// Apply an edit and verify flash readback; other device managers must be closed.
    Apply { edit: PathBuf },
    /// Restore the original profile from a previously applied edit.
    Restore { edit: PathBuf },
    /// Explicitly select execution mode: 1 onboard, 2 host. No flash write.
    Mode {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=2))]
        value: u8,
    },
    /// Activate an existing enabled profile sector. No flash write.
    Select { sector: u16 },
    /// Back up every user-memory sector to a NEW JSON file. No device writes.
    Backup { output: PathBuf },
}

fn parse_product_id(text: &str) -> Result<u16, String> {
    u16::from_str_radix(text.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    serde_json::from_slice(&std::fs::read(path)?)
        .with_context(|| format!("read {}", path.display()))
}

fn write_new_json<T: serde::Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let data = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create new file {}", path.display()))?;
    file.write_all(&data)?;
    file.sync_all()?;
    Ok(())
}

pub async fn run(args: GamingArgs) -> Result<()> {
    let route = DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: args.product_id,
    };
    match args.command {
        GamingCommand::Inspect { input } => {
            let backup: gaming::GamingBackup = if let Some(path) = input {
                read_json(&path)?
            } else {
                gaming::backup(&*openlogi_hid::host::backend(), &route).await?
            };
            print_inspection(&backup)?;
        }
        GamingCommand::Backup { output } => {
            let backup = gaming::backup(&*openlogi_hid::host::backend(), &route).await?;
            write_new_json(&output, &backup)?;
            println!(
                "Saved {} sectors to {} (device unchanged)",
                backup.sectors.len(),
                output.display()
            );
        }
        GamingCommand::Prepare {
            original,
            patch,
            sector,
            output,
        } => {
            let edit = gaming::ProfileEdit {
                schema_version: 1,
                original: read_json(&original)?,
                sector,
                patch: read_json(&patch)?,
            };
            edit.render()?;
            write_new_json(&output, &edit)?;
            println!(
                "Prepared {}. No hardware accessed. Use gaming preview before apply.",
                output.display()
            );
        }
        GamingCommand::Preview { edit } => preview_edit(&edit)?,
        GamingCommand::Mode { value } => {
            ensure_no_competing_managers()?;
            gaming::set_mode(&*openlogi_hid::host::backend(), &route, value).await?;
            println!("Execution mode verified: {value}");
        }
        GamingCommand::Select { sector } => {
            ensure_no_competing_managers()?;
            gaming::select_profile(&*openlogi_hid::host::backend(), &route, sector).await?;
            println!("Active profile verified: {sector}");
        }
        GamingCommand::Restore { edit } => {
            let edit: gaming::ProfileEdit = read_json(&edit)?;
            anyhow::ensure!(
                edit.original.route == route,
                "--product-id differs from the captured device"
            );
            ensure_no_competing_managers()?;
            let changed =
                gaming::restore_profile_edit(&*openlogi_hid::host::backend(), &edit).await?;
            println!(
                "{}",
                if changed {
                    "Original profile restored and verified."
                } else {
                    "Original already present; no writes."
                }
            );
        }
        GamingCommand::Apply { edit } => {
            let edit: gaming::ProfileEdit = read_json(&edit)?;
            anyhow::ensure!(
                edit.original.route == route,
                "--product-id differs from the captured device"
            );
            ensure_no_competing_managers()?;
            let changed =
                gaming::apply_profile_edit(&*openlogi_hid::host::backend(), &edit).await?;
            println!(
                "{}",
                if changed {
                    "Profile programmed and every byte verified. Re-select the profile to activate it."
                } else {
                    "No changes; no flash writes were sent."
                }
            );
        }
    }
    Ok(())
}

fn ensure_no_competing_managers() -> Result<()> {
    openlogi_hid::gaming_guard::ensure_exclusive(openlogi_hid::gaming_guard::Writer::Direct)
        .map_err(anyhow::Error::msg)
}

fn preview_edit(edit: &PathBuf) -> Result<()> {
    let edit: gaming::ProfileEdit = read_json(edit)?;
    let replacement = edit.render()?;
    let before = edit.original.profiles()?;
    let mut after = edit.original.clone();
    after.sectors[usize::from(edit.sector)] = replacement;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "sector": edit.sector,
            "before": before.iter().find(|p| p.sector == edit.sector),
            "after": after.profiles()?.iter().find(|p| p.sector == edit.sector),
            "hardware_verified": false,
        }))?
    );
    Ok(())
}

fn print_inspection(backup: &gaming::GamingBackup) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1, "route": backup.route, "mode": backup.mode,
            "active_profile": backup.active_profile, "memory_model": backup.memory_model,
            "profile_format": backup.profile_format, "macro_format": backup.macro_format,
            "profile_count": backup.profile_count, "button_count": backup.button_count,
            "sector_count": backup.sectors.len(), "sector_size": backup.sector_size,
            "profiles": backup.profiles().context("decode supported profile layout")?,
        }))?
    );
    Ok(())
}
