//! `openlogi diag mouse-buttons` — print which HID++ `0x8110` slot each
//! button of a G-series mouse lands on, the data a `GamingLayout` needs.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::{OnboardMode, WriteError};

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct MouseButtonsArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// How long to listen for presses, in seconds. The spy stops afterwards.
    #[arg(long, value_name = "SECS", default_value_t = 60)]
    pub duration: u64,
}

pub async fn run(args: MouseButtonsArgs) -> Result<()> {
    // 0x8110 = MouseButtonSpy, the G-series button report.
    let (route, name) = select_device(args.device.as_deref(), &[0x8110]).await?;
    println!("device: {name} ({route})");

    // In host mode the OpenLogi agent or G HUB owns the spy, and stopping it
    // here would leave their captured buttons dead.
    match openlogi_hid::get_onboard(&route).await {
        Ok(state) if state.mode == OnboardMode::Host => bail!(
            "the mouse is in host mode, so another program owns its button spy; \
             quit the OpenLogi agent and G HUB, then run this again"
        ),
        Ok(_) | Err(WriteError::FeatureUnsupported { .. }) => {}
        Err(error) => tracing::warn!("could not read the onboard mode, assuming onboard: {error}"),
    }

    let mut watch = openlogi_hid::watch_button_spy(&route)
        .await
        .context("start the HID++ 0x8110 button spy")?;
    println!("  slots: {}", watch.mapping().len());
    println!("  host-mode mapping: {}", MappingDisplay(watch.mapping()));
    println!(
        "  press and release each button, one at a time ({} s)",
        args.duration
    );

    let deadline = tokio::time::sleep(Duration::from_secs(args.duration));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => break,
            mask = watch.next() => {
                let Some(mask) = mask else {
                    bail!("the mouse stopped answering");
                };
                println!("  {mask:#06x}  {}", HeldSlots(mask));
            }
        }
    }
    watch
        .stop()
        .await
        .context("stop the HID++ 0x8110 button spy")?;
    println!("  spy stopped");
    Ok(())
}

/// The slots a spy mask holds, with the G number G HUB prints for each:
/// slot `n` is `G{n + 1}`.
struct HeldSlots(u16);

impl fmt::Display for HeldSlots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return write!(f, "released");
        }
        let mut separator = "";
        for slot in (0..u16::BITS).filter(|slot| self.0 & (1 << slot) != 0) {
            write!(f, "{separator}slot {slot} (G{})", slot + 1)?;
            separator = ", ";
        }
        Ok(())
    }
}

/// One host-mode HID button per slot, as hex.
struct MappingDisplay<'a>(&'a [u8]);

impl fmt::Display for MappingDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return write!(f, "-");
        }
        let mut separator = "";
        for code in self.0 {
            write!(f, "{separator}{code:02x}")?;
            separator = " ";
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_slots_name_each_slot_and_its_g_number() {
        assert_eq!(HeldSlots(0x0020).to_string(), "slot 5 (G6)");
        assert_eq!(HeldSlots(0x0401).to_string(), "slot 0 (G1), slot 10 (G11)");
        assert_eq!(HeldSlots(0x8000).to_string(), "slot 15 (G16)");
        assert_eq!(HeldSlots(0).to_string(), "released");
    }

    #[test]
    fn the_mapping_prints_one_hex_byte_per_slot() {
        assert_eq!(
            MappingDisplay(&[1, 2, 3, 4, 5, 0, 16]).to_string(),
            "01 02 03 04 05 00 10"
        );
        assert_eq!(MappingDisplay(&[]).to_string(), "-");
    }
}
