//! `openlogi diag features` — dump the device's HID++ feature table.
//!
//! Useful for figuring out *which* DPI / SmartShift / etc. feature ID a
//! given peripheral exposes when the default wrappers (0x2201, 0x2111)
//! aren't recognised.

use anyhow::Result;
use clap::Args;
use openlogi_hid::DeviceRoute;

#[derive(Debug, Args)]
pub struct FeaturesArgs {}

pub async fn run(_args: FeaturesArgs) -> Result<()> {
    let inventories = openlogi_hid::enumerate().await?;
    let mut any = false;
    for inv in &inventories {
        for paired in inv.paired.iter().filter(|p| p.online) {
            any = true;
            let route = match &inv.receiver.unique_id {
                Some(uid) => DeviceRoute::Bolt {
                    receiver_uid: uid.clone(),
                    slot: paired.slot,
                },
                None => DeviceRoute::Direct {
                    vendor_id: inv.receiver.vendor_id,
                    product_id: inv.receiver.product_id,
                },
            };
            let name = paired
                .codename
                .clone()
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            println!("device: {name} ({route})");
            match openlogi_hid::dump_features(&route).await {
                Ok(entries) => {
                    println!("  {:>4}  {:>6}  {:<7}", "idx", "id", "ver");
                    for (idx, entry) in entries.iter().enumerate() {
                        println!("  {:>4}  0x{:04x}  v{}", idx, entry.id, entry.version);
                    }
                    println!("  ({} feature entries)\n", entries.len());
                }
                Err(e) => println!("  dump failed: {e:#}\n"),
            }
        }
    }
    if !any {
        println!("no online HID++ devices found");
    }
    Ok(())
}
