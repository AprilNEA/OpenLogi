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
        println!("No Logitech HID++ devices found.");
        println!();
        println!("Notes:");
        println!("  - On macOS, quit Logi Options+ first — both apps fight over HID++ access.");
        println!(
            "  - A Bluetooth-direct mouse (e.g. Lift, Signature) needs Input Monitoring \
             permission: System Settings → Privacy & Security → Input Monitoring."
        );
        println!(
            "  - hidpp 0.2 only recognises Logi Bolt receivers (PID 0xC548); other \
             receivers (Unifying) aren't surfaced yet."
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
    let serial = m.serial_number.as_deref().unwrap_or("—");
    format!(
        "     model_ids=[{ids}] ext={:02x} serial={serial} unit_id={unit} transports={transports}",
        m.extended_model_id
    )
}

#[cfg(test)]
mod format_tests {
    use openlogi_core::device::{BatteryLevel, BatteryStatus, DeviceKind, DeviceTransports};

    use super::{BatteryInfo, DeviceModelInfo};
    use super::{PairedDevice, format_battery, format_device, format_model};

    fn base_device() -> PairedDevice {
        PairedDevice {
            slot: 1,
            codename: Some("MX Master 3S".to_string()),
            wpid: Some(0x4082),
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        }
    }

    #[test]
    fn online_device_uses_filled_dot_and_reports_fields() {
        let d = base_device();
        let out = format_device(&d);
        assert_eq!(out, "slot 1 ● MX Master 3S (mouse, wpid=4082, battery=—)");
    }

    #[test]
    fn offline_device_uses_hollow_dot() {
        let mut d = base_device();
        d.online = false;
        let out = format_device(&d);
        assert!(out.starts_with("slot 1 ○ "));
    }

    #[test]
    fn missing_codename_and_wpid_fall_back_to_placeholders() {
        let mut d = base_device();
        d.codename = None;
        d.wpid = None;
        let out = format_device(&d);
        assert_eq!(out, "slot 1 ● Unknown device (mouse, wpid=?, battery=—)");
    }

    #[test]
    fn battery_info_is_embedded_when_present() {
        let mut d = base_device();
        d.battery = Some(BatteryInfo {
            percentage: 42,
            level: BatteryLevel::Low,
            status: BatteryStatus::Discharging,
        });
        let out = format_device(&d);
        assert!(out.contains("battery=42% low (discharging)"));
    }

    #[test]
    fn battery_status_debug_names_are_lowercased_verbatim() {
        // `ChargingSlow`'s Debug form has no separator; lowercasing alone
        // yields "chargingslow", not "charging_slow" or "charging slow".
        let b = BatteryInfo {
            percentage: 10,
            level: BatteryLevel::Critical,
            status: BatteryStatus::ChargingSlow,
        };
        assert_eq!(format_battery(&b), "battery=10% critical (chargingslow)");
    }

    fn base_model() -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 1,
            serial_number: None,
            unit_id: [0x00, 0x01, 0x02, 0x03],
            transports: DeviceTransports::default(),
            model_ids: [0xb042, 0, 0],
            extended_model_id: 0x02,
        }
    }

    #[test]
    fn model_with_no_transports_shows_placeholder_and_missing_serial() {
        let m = base_model();
        let out = format_model(&m);
        assert_eq!(
            out,
            "     model_ids=[b042,0000,0000] ext=02 serial=— unit_id=00010203 transports=—"
        );
    }

    #[test]
    fn model_transports_join_in_declared_field_order() {
        let mut m = base_model();
        m.transports = DeviceTransports {
            usb: true,
            equad: false,
            btle: true,
            bluetooth: true,
        };
        m.serial_number = Some("SN123".to_string());
        let out = format_model(&m);
        assert!(out.contains("transports=usb+btle+bt"));
        assert!(out.contains("serial=SN123"));
    }
}
