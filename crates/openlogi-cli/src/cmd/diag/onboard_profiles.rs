//! `openlogi diag onboard-profiles` — dump the HID++ `0x8100 OnboardProfiles`
//! info, and optionally one raw onboard-memory sector.
//!
//! Read-only. G-series gaming mice (G502 X, G502 X LIGHTSPEED, ...) expose no
//! `ReprogControls` (`0x1b00`–`0x1b04`) at all, so the desktop app's Buttons
//! panel never appears for them; they use `0x8100` for on-device profile and
//! button storage instead. `--sector` exists to capture real profile bytes
//! from hardware — see `hidpp::feature::onboard_profiles` for what is (and
//! is not) decoded from them yet.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::{ButtonBinding, parse_profile_directory};

use crate::cmd::diag::select_device;

/// Byte offset where a profile sector's button-binding table was observed to
/// start on a G502 X LIGHTSPEED and a G502 LIGHTSPEED — an empirical
/// observation, not a cited protocol constant. See
/// `hidpp::feature::onboard_profiles` module docs.
const OBSERVED_BUTTON_TABLE_OFFSET: usize = 32;

/// Safety cap on how many button-binding entries to decode past the
/// observed offset, so a wrong offset guess on an unfamiliar device prints a
/// bounded amount of (possibly garbage) output instead of walking the whole
/// sector.
const MAX_DECODED_BUTTON_ENTRIES: usize = 32;

#[derive(Debug, Args)]
pub struct OnboardProfilesArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Also dump this onboard-memory sector's raw bytes (e.g. `0` for the
    /// profile directory every device libratbag documents carries).
    #[arg(long, value_name = "SECTOR")]
    pub sector: Option<u16>,
}

pub async fn run(args: OnboardProfilesArgs) -> Result<()> {
    // 0x8100 = OnboardProfiles.
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let info = openlogi_hid::dump_onboard_profiles_info(&route)
        .await
        .context("dump HID++ 0x8100 onboard-profiles info")?;
    println!("  memory_model_id:    {}", info.memory_model_id);
    println!("  profile_format_id:  {}", info.profile_format_id);
    println!("  macro_format_id:    {}", info.macro_format_id);
    println!("  profile_count:      {}", info.profile_count);
    println!("  profile_count_oob:  {}", info.profile_count_oob);
    println!("  button_count:       {}", info.button_count);
    println!("  sector_count:       {}", info.sector_count);
    println!("  sector_size:        {}", info.sector_size);
    println!("  mechanical_layout:  {}", info.mechanical_layout);
    println!("  various_info:       {}", info.various_info);

    if let Some(sector) = args.sector {
        let data = openlogi_hid::dump_onboard_profiles_sector(&route, sector, info.sector_size)
            .await
            .with_context(|| format!("read HID++ 0x8100 onboard-memory sector {sector:#06x}"))?;
        println!("  sector {sector:#06x} ({} bytes):", data.len());
        for (row, chunk) in data.chunks(16).enumerate() {
            print!("    {:04x}:", row * 16);
            for byte in chunk {
                print!(" {byte:02x}");
            }
            println!();
        }

        if sector == 0 {
            let entries = parse_profile_directory(&data);
            println!("  profile directory ({} populated entries):", entries.len());
            for (i, entry) in entries.iter().enumerate() {
                println!(
                    "    profile {}: sector {:#06x}, flag {:#04x}",
                    i + 1,
                    entry.address,
                    entry.flag
                );
            }
        } else if let Some(table) = data.get(OBSERVED_BUTTON_TABLE_OFFSET..) {
            println!(
                "  button bindings from offset {OBSERVED_BUTTON_TABLE_OFFSET:#04x} \
                 (best-effort — see hidpp::feature::onboard_profiles docs):"
            );
            let (chunks, _) = table.as_chunks::<4>();
            for (i, &entry) in chunks.iter().take(MAX_DECODED_BUTTON_ENTRIES).enumerate() {
                match ButtonBinding::parse(entry) {
                    Ok(ButtonBinding::Disabled) => {
                        println!("    [{i}] disabled — stopping");
                        break;
                    }
                    Ok(binding) => println!("    [{i}] {binding:?}"),
                    Err(_) => {
                        println!(
                            "    [{i}] unrecognized entry {:02x} {:02x} {:02x} {:02x}",
                            entry[0], entry[1], entry[2], entry[3]
                        );
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}
