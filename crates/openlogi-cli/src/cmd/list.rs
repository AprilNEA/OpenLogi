use anyhow::{Context, Result};
use clap::Args;
use openlogi_core::device::{BatteryInfo, DeviceInventory, DeviceModelInfo, PairedDevice};

#[derive(Debug, Args)]
pub struct ListArgs {}

pub async fn run(_args: ListArgs) -> Result<()> {
    let inventories = openlogi_hid::enumerate()
        .await
        .context("failed to enumerate HID++ devices")?;

    if inventories.is_empty() {
        println!("No Logitech HID++ receivers found.");
        println!();
        println!("Notes:");
        println!("  - On macOS, quit Logi Options+ first — both apps fight over HID++ access.");
        println!("  - hidpp 0.2 only recognises Logi Bolt receivers (PID 0xC548).");
        println!(
            "  - Bluetooth-direct and USB-wired devices are enumerated; make sure the device is \
             connected and awake."
        );
        std::process::exit(2);
    }

    for (i, inv) in inventories.iter().enumerate() {
        if i != 0 {
            println!();
        }
        print_inventory(inv);
    }

    Ok(())
}

fn print_inventory(inv: &DeviceInventory) {
    let uid = inv.receiver.unique_id.as_deref().unwrap_or("—");
    println!(
        "{} ({}, vid={:04x} pid={:04x})",
        inv.receiver.name, uid, inv.receiver.vendor_id, inv.receiver.product_id
    );

    if inv.paired.is_empty() {
        println!("  └─ no paired devices");
        return;
    }

    let last = inv.paired.len() - 1;
    for (i, d) in inv.paired.iter().enumerate() {
        let prefix = if i == last { "  └─" } else { "  ├─" };
        println!("{prefix} {}", format_device(d));
        if let Some(model) = d.model_info.as_ref() {
            let cont = if i == last { "     " } else { "  │  " };
            println!("{cont}{}", format_model(model));
        }
    }
}

fn format_device(d: &PairedDevice) -> String {
    let dot = if d.online { "●" } else { "○" };
    let codename = d.codename.as_deref().unwrap_or("Unknown device");
    let wpid = d
        .wpid
        .map_or_else(|| "wpid=?".to_string(), |w| format!("wpid={w:04x}"));
    let battery = d
        .battery
        .as_ref()
        .map_or_else(|| "battery=—".to_string(), format_battery);
    let kind = format!("{:?}", d.kind).to_lowercase();
    format!(
        "slot {} {dot} {codename} ({kind}, {wpid}, {battery})",
        d.slot
    )
}

fn format_battery(b: &BatteryInfo) -> String {
    let level = format!("{:?}", b.level).to_lowercase();
    let status = format!("{:?}", b.status).to_lowercase();
    format!("battery={}% {level} ({status})", b.percentage)
}

fn format_model(m: &DeviceModelInfo) -> String {
    let transports = {
        let mut t = Vec::new();
        if m.transports.usb {
            t.push("usb");
        }
        if m.transports.equad {
            t.push("equad");
        }
        if m.transports.btle {
            t.push("btle");
        }
        if m.transports.bluetooth {
            t.push("bt");
        }
        if t.is_empty() {
            "—".to_string()
        } else {
            t.join("+")
        }
    };
    let ids = m
        .model_ids
        .iter()
        .map(|id| format!("{id:04x}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut unit = String::with_capacity(8);
    for b in m.unit_id {
        use std::fmt::Write as _;
        let _ = write!(unit, "{b:02x}");
    }
    format!(
        "     model_ids=[{ids}] ext={:02x} unit_id={unit} transports={transports}",
        m.extended_model_id
    )
}
