//! `openlogi diag mouse-buttons` — dump HID++ 0x8110 mouse button spy state.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::host::{dump_mouse_button_filter, watch_mouse_button_filter};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct MouseButtonsArgs {
    /// Start the button spy and print each event mask until Ctrl-C.
    #[arg(long)]
    pub watch: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: MouseButtonsArgs) -> Result<()> {
    // 0x8110 = MouseButtonFilter (Mouse Button Spy).
    let (route, name) = select_device(args.device.as_deref(), &[0x8110]).await?;
    println!("device: {name} ({route})");

    let dump = dump_mouse_button_filter(&route)
        .await
        .context("dump HID++ 0x8110 mouse button filter")?;
    println!("  count: {}", dump.button_count);
    println!("  mapping: {}", mapping_hex(&dump.mapping));

    if !args.watch {
        return Ok(());
    }

    println!("  watching mouse button spy (Ctrl-C to stop)");
    watch_mouse_button_filter(
        &route,
        |mask| {
            println!("  mask: 0b{mask:016b} {mask:#06x}");
        },
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
    .context("watch HID++ 0x8110 mouse button spy")?;
    Ok(())
}

fn mapping_hex(mapping: &[u8]) -> String {
    if mapping.is_empty() {
        return "-".to_owned();
    }
    mapping
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::mapping_hex;

    #[test]
    fn formats_empty_mapping_as_dash() {
        assert_eq!(mapping_hex(&[]), "-");
    }

    #[test]
    fn formats_mapping_as_spaced_hex() {
        assert_eq!(mapping_hex(&[1, 2, 0, 16]), "01 02 00 10");
    }
}
