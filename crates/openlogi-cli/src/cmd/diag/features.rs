//! `openlogi diag features` — dump the device's HID++ feature table.
//!
//! Useful for figuring out *which* DPI / SmartShift / etc. feature ID a
//! given peripheral exposes when the default wrappers (0x2201, 0x2111)
//! aren't recognised.

use anyhow::Result;
use clap::Args;
use openlogi_hid::{DeviceRoute, FeatureType, FirmwareEntityEntry};

#[derive(Debug, Args)]
pub struct FeaturesArgs {}

pub async fn run(_args: FeaturesArgs) -> Result<()> {
    let inventories = openlogi_hid::enumerate().await?;
    let mut any = false;
    for inv in &inventories {
        for paired in inv.paired.iter().filter(|p| p.online) {
            any = true;
            let route =
                DeviceRoute::device_route_for(inv, paired.slot).unwrap_or(DeviceRoute::Direct {
                    vendor_id: inv.receiver.vendor_id,
                    product_id: inv.receiver.product_id,
                });
            let name = paired
                .codename
                .clone()
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            println!("device: {name} ({route})");
            match openlogi_hid::dump_features(&route).await {
                Ok(entries) => {
                    println!("  {:>4}  {:>6}  {:<4}  flags", "idx", "id", "ver");
                    for (idx, entry) in entries.iter().enumerate() {
                        let mut flags = Vec::new();
                        if entry.typ.contains(FeatureType::OBSOLETE) {
                            flags.push("obsolete");
                        }
                        if entry.typ.contains(FeatureType::HIDDEN) {
                            flags.push("hidden");
                        }
                        if entry.typ.contains(FeatureType::ENGINEERING) {
                            flags.push("engineering");
                        }
                        println!(
                            "  {:>4}  0x{:04x}  v{:<3}  {}",
                            idx,
                            entry.id,
                            entry.version,
                            flags.join(",")
                        );
                    }
                    println!("  ({} feature entries)\n", entries.len());
                }
                Err(e) => println!("  dump failed: {e:#}\n"),
            }
            match openlogi_hid::dump_firmware_entities(&route).await {
                Ok(entries) => {
                    for entry in &entries {
                        println!("  {}", format_firmware_entity(entry));
                    }
                }
                Err(e) => println!("  firmware dump failed: {e:#}"),
            }
            println!();
        }
    }
    if !any {
        println!("no online HID++ devices found");
    }
    Ok(())
}

/// Render one firmware entity as a single diagnostics line.
///
/// An entity the device declared but whose record did not parse is reported
/// rather than dropped: a device that cannot describe one of its own firmware
/// images is exactly what a bug report needs to say.
fn format_firmware_entity(entry: &FirmwareEntityEntry) -> String {
    let Some(version) = entry.version.as_deref() else {
        let reason = entry.error.as_deref().unwrap_or("unparsed");
        return format!("fw {}: unreadable ({reason})", entry.index);
    };
    let kind = entry
        .kind
        .map_or_else(|| "unknown".to_string(), |kind| format!("{kind:?}"));
    let pid = entry
        .transport_pid
        .map_or_else(String::new, |pid| format!(" pid={pid:04x}"));
    let active = if entry.active { " [active]" } else { "" };
    format!("fw {}: {kind} {version}{pid}{active}", entry.index)
}

#[cfg(test)]
mod tests {
    use openlogi_hid::{DeviceEntityType, FirmwareEntityEntry};

    use super::format_firmware_entity;

    fn entry() -> FirmwareEntityEntry {
        FirmwareEntityEntry {
            index: 1,
            kind: Some(DeviceEntityType::MainApplication),
            version: Some("MPM17.00_B0008".to_string()),
            transport_pid: Some(0xc08d),
            active: true,
            error: None,
        }
    }

    #[test]
    fn the_running_entity_is_marked_active() {
        assert_eq!(
            format_firmware_entity(&entry()),
            "fw 1: MainApplication MPM17.00_B0008 pid=c08d [active]"
        );
    }

    #[test]
    fn a_dormant_entity_carries_no_marker() {
        let mut e = entry();
        e.active = false;
        assert_eq!(
            format_firmware_entity(&e),
            "fw 1: MainApplication MPM17.00_B0008 pid=c08d"
        );
    }

    #[test]
    fn an_entity_without_a_pid_omits_the_field() {
        let mut e = entry();
        e.transport_pid = None;
        assert_eq!(
            format_firmware_entity(&e),
            "fw 1: MainApplication MPM17.00_B0008 [active]"
        );
    }

    /// A G502 LIGHTSPEED declares three entities and its radio stack does not
    /// parse. Reporting the gap is the point: dropping the row would claim the
    /// device has two firmware images when it says it has three.
    #[test]
    fn an_unreadable_entity_reports_the_reason() {
        let e = FirmwareEntityEntry {
            index: 2,
            kind: None,
            version: None,
            transport_pid: None,
            active: false,
            error: Some("UnsupportedResponse".to_string()),
        };
        assert_eq!(
            format_firmware_entity(&e),
            "fw 2: unreadable (UnsupportedResponse)"
        );
    }

    #[test]
    fn an_unreadable_entity_with_no_reason_still_renders() {
        let e = FirmwareEntityEntry {
            index: 2,
            kind: None,
            version: None,
            transport_pid: None,
            active: false,
            error: None,
        };
        assert_eq!(format_firmware_entity(&e), "fw 2: unreadable (unparsed)");
    }
}
