//! `openlogi diag profiles` — dump HID++ `0x8100` onboard profiles.

use anyhow::{Context, Result};
use clap::Args;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct ProfilesArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: ProfilesArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x8100]).await?;
    println!("device: {name} ({route})");

    let dump = openlogi_hid::dump_onboard_profiles(&route)
        .await
        .context("dump HID++ 0x8100 onboard profiles")?;
    let desc = dump.description;
    println!(
        "  memory=0x{:02x} format=0x{:02x} macro=0x{:02x} profiles={} rom={} buttons={} sectors={} sector_size={}",
        desc.memory_model_id,
        desc.profile_format_id,
        desc.macro_format_id,
        desc.profile_count,
        desc.rom_profile_count,
        desc.button_count,
        desc.sector_count,
        desc.sector_size
    );
    println!(
        "  mode={:?} active_profile={} (raw {})",
        dump.mode, dump.active_profile, dump.active_profile_raw
    );
    for (i, entry) in dump.directory.iter().enumerate() {
        println!(
            "  dir[{i}]: sector=0x{:04x} enabled={}",
            entry.address, entry.enabled
        );
    }
    if dump.active_buttons.is_empty() {
        println!("  (no packed button table decoded for this profile format)");
    } else {
        println!("  {:>4}  {:<16}  binding", "g", "button");
        for slot in dump.active_buttons {
            let name = slot
                .button
                .map_or_else(|| "-".to_string(), |button| button.label().to_string());
            let bytes = slot.binding.bytes;
            println!(
                "  G{:<3}  {:<16}  {:02x} {:02x} {:02x} {:02x}  {}",
                slot.index + 1,
                name,
                bytes[0],
                bytes[1],
                bytes[2],
                bytes[3],
                describe_binding(bytes)
            );
        }
    }
    if dump.dpi_presets.is_empty() {
        println!("  dpi: (none decoded)");
    } else {
        let presets: Vec<String> = dump.dpi_presets.iter().map(ToString::to_string).collect();
        println!("  dpi: {}", presets.join(", "));
    }
    if dump.leds.is_empty() {
        println!("  leds: (none decoded)");
    } else {
        for (i, led) in dump.leds.iter().enumerate() {
            let (r, g, b) = led.color.components();
            println!(
                "  led[{i}]: {:?} #{r:02x}{g:02x}{b:02x} brightness={}",
                led.mode, led.brightness
            );
        }
    }
    Ok(())
}

fn describe_binding(bytes: [u8; 4]) -> &'static str {
    match bytes {
        [0xff, ..] => "disabled",
        [0x80, 0x01, 0x00, 0x01] => "mouse 1 (left)",
        [0x80, 0x01, 0x00, 0x02] => "mouse 2 (right)",
        [0x80, 0x01, 0x00, 0x04] => "mouse 3 (middle)",
        [0x80, 0x01, 0x00, 0x08] => "mouse 4 (back)",
        [0x80, 0x01, 0x00, 0x10] => "mouse 5 (forward)",
        [0x80, 0x02, ..] => "hid keyboard",
        [0x80, 0x03, ..] => "hid consumer",
        [0x90, 0x01, ..] => "tilt left",
        [0x90, 0x02, ..] => "tilt right",
        [0x90, 0x03, ..] => "next DPI",
        [0x90, 0x04, ..] => "prev DPI",
        [0x90, 0x05, ..] => "cycle DPI",
        [0x90, 0x07, ..] => "DPI shift",
        [0x90, 0x0a, ..] => "cycle profile",
        [0x90, 0x0b, ..] => "G-shift",
        [0x00, ..] => "macro",
        _ => "other",
    }
}
