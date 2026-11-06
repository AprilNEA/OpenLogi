//! `openlogi diag features` — dump the device's HID++ feature table.
//!
//! Useful for figuring out *which* DPI / SmartShift / etc. feature ID a
//! given peripheral exposes when the default wrappers (0x2201, 0x2111)
//! aren't recognised.

use std::fmt;

use anyhow::Result;
use clap::Args;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{DeviceRoute, FeatureType, FirmwareEntity};

#[derive(Debug, Args)]
pub struct FeaturesArgs {}

pub async fn run(_args: FeaturesArgs) -> Result<()> {
    let inventories = openlogi_hid::enumerate().await?;
    let sections = sections(&inventories);
    for section in &sections {
        let route = &section.route;
        println!("device: {} ({route})", section.name);
        match section.read {
            Read::Tables => dump_tables(route).await,
            Read::Hidpp10 => {
                println!("  HID++ 1.0 device: no HID++ 2.0 feature table or firmware entities");
            }
        }
        println!();
    }
    if sections.is_empty() {
        println!("no online HID++ devices found");
    }
    Ok(())
}

/// One online paired device, and what `diag features` reads from it.
#[derive(Debug, PartialEq, Eq)]
struct Section {
    /// The receiver's codename for the device, or `Slot <n>` without one.
    name: String,
    route: DeviceRoute,
    read: Read,
}

/// What `diag features` reads from one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// The HID++ 2.0 feature table and the `0x0003` firmware entities.
    Tables,
    /// Neither: the inventory probe found the device answers as HID++ 1.0
    /// ([`PairedDevice::hidpp10`]), which has no feature table. A dump would
    /// fail with "did not respond to HID++" although the device answered
    /// (#1554).
    ///
    /// [`PairedDevice::hidpp10`]: openlogi_core::device::PairedDevice::hidpp10
    Hidpp10,
}

/// Every online paired device in `inventories`, in enumeration order.
fn sections(inventories: &[DeviceInventory]) -> Vec<Section> {
    inventories
        .iter()
        .flat_map(|inv| {
            inv.paired
                .iter()
                .filter(|paired| paired.online)
                .map(move |paired| Section {
                    name: paired
                        .codename
                        .clone()
                        .unwrap_or_else(|| format!("Slot {}", paired.slot)),
                    route: DeviceRoute::for_slot(inv, paired.slot).unwrap_or(DeviceRoute::Direct {
                        vendor_id: inv.receiver.vendor_id,
                        product_id: inv.receiver.product_id,
                    }),
                    read: if paired.hidpp10 {
                        Read::Hidpp10
                    } else {
                        Read::Tables
                    },
                })
        })
        .collect()
}

/// Print the feature table and the firmware entities of the device `route`
/// reaches, or why each one failed.
async fn dump_tables(route: &DeviceRoute) {
    match openlogi_hid::dump_features(route).await {
        Ok(entries) => {
            println!("  {:>4}  {:>6}  {:<4}  flags", "idx", "id", "ver");
            for entry in &entries {
                println!(
                    "  {:>4}  0x{:04x}  v{:<3}  {}",
                    entry.index,
                    entry.id,
                    entry.version,
                    FeatureFlagsDisplay(entry.typ)
                );
            }
            println!("  ({} feature entries)\n", entries.len());
        }
        Err(e) => println!("  dump failed: {e:#}\n"),
    }
    match openlogi_hid::dump_firmware_entities(route).await {
        Ok(entries) => {
            for entry in &entries {
                println!("  {}", FirmwareEntityDisplay(entry));
            }
        }
        Err(e) => println!("  firmware dump failed: {e:#}"),
    }
}

/// Every feature-type flag the device set, as a comma-separated list.
///
/// All five known flags are named, and whatever bits are left over are printed
/// as a raw mask: the HID++ parser retains unknown bits (`from_bits_retain`),
/// and an undocumented bit on a new device is exactly what this column exists
/// to surface. Dropping either would make the column a partial view of a
/// response it is meant to report verbatim.
struct FeatureFlagsDisplay(FeatureType);

impl fmt::Display for FeatureFlagsDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMED: [(FeatureType, &str); 5] = [
            (FeatureType::OBSOLETE, "obsolete"),
            (FeatureType::HIDDEN, "hidden"),
            (FeatureType::ENGINEERING, "engineering"),
            (
                FeatureType::MANUFACTURING_DEACTIVATABLE,
                "manufacturing-deactivatable",
            ),
            (
                FeatureType::COMPLIANCE_DEACTIVATABLE,
                "compliance-deactivatable",
            ),
        ];

        let mut separator = "";
        for (flag, name) in NAMED {
            if self.0.contains(flag) {
                write!(f, "{separator}{name}")?;
                separator = ",";
            }
        }

        let unknown = self.0.bits() & !FeatureType::all().bits();
        if unknown != 0 {
            write!(f, "{separator}unknown=0x{unknown:02x}")?;
        }

        Ok(())
    }
}

/// Render one firmware entity as a single diagnostics line.
///
/// An entity the device declared but could not describe is reported rather
/// than dropped: a device that cannot describe one of its own firmware images
/// is exactly what a bug report needs to say.
struct FirmwareEntityDisplay<'a>(&'a FirmwareEntity);

impl fmt::Display for FirmwareEntityDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            FirmwareEntity::Readable { index, info } => {
                write!(
                    f,
                    "fw {index}: {:?} {}{:02}.{:02}_B{:04}",
                    info.kind, info.prefix, info.number, info.revision, info.build
                )?;
                if let Some(pid) = info.transport_pid {
                    write!(f, " pid={pid:04x}")?;
                }
                if info.active {
                    write!(f, " [active]")?;
                }

                // The raw region is printed whole whenever it holds a nonzero
                // byte that `pid=` does not already show, so the line never
                // drops what the device sent. All zero is how a device fills
                // bytes it does not implement.
                let shown = if info.transport_pid.is_some() { 2 } else { 0 };
                if info.transport_specific_info[shown..]
                    .iter()
                    .any(|&b| b != 0)
                {
                    write!(f, " transport-info=")?;
                    for byte in info.transport_specific_info {
                        write!(f, "{byte:02x}")?;
                    }
                }

                Ok(())
            }
            FirmwareEntity::Unreadable { index, error } => {
                write!(f, "fw {index}: unreadable ({error})")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_core::device::{DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo};
    use openlogi_hid::{
        DeviceEntityType, DeviceRoute, FeatureType, FirmwareEntity, FirmwareEntityInfo,
        HidppOperation, WriteError,
    };

    use super::{FeatureFlagsDisplay, FirmwareEntityDisplay, Read, Section, sections};

    fn entry() -> FirmwareEntity {
        FirmwareEntity::Readable {
            index: 1,
            info: FirmwareEntityInfo {
                kind: DeviceEntityType::MainApplication,
                prefix: "MPM".to_owned(),
                number: 17,
                revision: 0,
                build: 8,
                active: true,
                transport_pid: Some(0xc08d),
                transport_specific_info: [0xc0, 0x8d, 0, 0, 0, 0, 0],
            },
        }
    }

    fn info(entry: &mut FirmwareEntity) -> &mut FirmwareEntityInfo {
        match entry {
            FirmwareEntity::Readable { info, .. } => info,
            FirmwareEntity::Unreadable { .. } => panic!("entry() builds a readable entity"),
        }
    }

    #[test]
    fn the_running_entity_is_marked_active() {
        assert_eq!(
            FirmwareEntityDisplay(&entry()).to_string(),
            "fw 1: MainApplication MPM17.00_B0008 pid=c08d [active]"
        );
    }

    #[test]
    fn a_dormant_entity_carries_no_marker() {
        let mut e = entry();
        info(&mut e).active = false;
        info(&mut e).transport_pid = None;
        info(&mut e).transport_specific_info = [0; 7];
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 1: MainApplication MPM17.00_B0008"
        );
    }

    /// A PID the protocol layer read is printed even when it is zero: that is
    /// still what the device said.
    #[test]
    fn a_zero_pid_is_reported_rather_than_hidden() {
        let mut e = entry();
        info(&mut e).transport_pid = Some(0);
        info(&mut e).transport_specific_info = [0; 7];
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 1: MainApplication MPM17.00_B0008 pid=0000 [active]"
        );
    }

    /// Bytes after the PID are device-specific and normally zero. When a
    /// device does fill them in, the whole region is printed.
    #[test]
    fn transport_bytes_beyond_the_pid_are_shown() {
        let mut e = entry();
        info(&mut e).transport_specific_info = [0xc0, 0x8d, 0x01, 0x02, 0x00, 0xff, 0x10];
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 1: MainApplication MPM17.00_B0008 pid=c08d [active] transport-info=c08d010200ff10"
        );
    }

    /// A G502 LIGHTSPEED's dormant bootloader sends `0xaaef` in bytes 9 and
    /// 10. It is not the PID of the link, so the line shows raw bytes and no
    /// `pid=`.
    #[test]
    fn an_uninterpreted_region_is_shown_raw_not_as_a_pid() {
        let e = FirmwareEntity::Readable {
            index: 0,
            info: FirmwareEntityInfo {
                kind: DeviceEntityType::Bootloader,
                prefix: "BOT".to_owned(),
                number: 92,
                revision: 0,
                build: 8,
                active: false,
                transport_pid: None,
                transport_specific_info: [0xaa, 0xef, 0, 0, 0, 0, 0],
            },
        };
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 0: Bootloader BOT92.00_B0008 transport-info=aaef0000000000"
        );
    }

    /// A G502 LIGHTSPEED declares three entities and its radio stack does not
    /// parse. Reporting the gap is the point: dropping the row would claim the
    /// device has two firmware images when it says it has three.
    #[test]
    fn an_unreadable_entity_reports_the_reason() {
        let e = FirmwareEntity::Unreadable {
            index: 2,
            error: WriteError::UnsupportedResponse {
                operation: HidppOperation::DumpFeatures,
                feature_hex: 0x0003,
            },
        };
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 2: unreadable (HID++ unsupported response during DumpFeatures for feature 0x0003)"
        );
    }

    #[test]
    fn a_feature_with_no_flags_renders_empty() {
        assert_eq!(FeatureFlagsDisplay(FeatureType::empty()).to_string(), "");
    }

    #[test]
    fn every_known_flag_is_named() {
        assert_eq!(
            FeatureFlagsDisplay(FeatureType::all()).to_string(),
            "obsolete,hidden,engineering,manufacturing-deactivatable,compliance-deactivatable"
        );
    }

    /// The two version-2 flags used to be dropped silently, which made the
    /// column a partial view of the device's own answer.
    #[test]
    fn the_version_two_flags_are_not_dropped() {
        assert_eq!(
            FeatureFlagsDisplay(FeatureType::HIDDEN | FeatureType::COMPLIANCE_DEACTIVATABLE)
                .to_string(),
            "hidden,compliance-deactivatable"
        );
    }

    /// `from_bits_retain` keeps bits this build does not know. A new device
    /// setting one is precisely the case this column is for, so the remainder
    /// is shown as a raw mask instead of vanishing.
    #[test]
    fn unknown_bits_are_reported_as_a_raw_mask() {
        assert_eq!(
            FeatureFlagsDisplay(FeatureType::from_bits_retain(0b0100_0100)).to_string(),
            "hidden,unknown=0x04"
        );
    }

    #[test]
    fn unknown_bits_alone_still_render() {
        assert_eq!(
            FeatureFlagsDisplay(FeatureType::from_bits_retain(0b0000_0011)).to_string(),
            "unknown=0x03"
        );
    }

    fn online(slot: u8) -> PairedDevice {
        PairedDevice {
            slot,
            codename: None,
            wpid: None,
            kind: DeviceKind::Mouse,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
            settings_revision: 0,
            hidpp10: false,
        }
    }

    fn slot(slot: u8) -> DeviceRoute {
        DeviceRoute::Unifying {
            receiver_uid: "receiver-uid".into(),
            slot,
        }
    }

    /// The receiver in #1554: HID++ 1.0 mice in slots 1 and 3, a HID++ 2.0
    /// keyboard in slot 2. Only the keyboard has tables to dump. Before, both
    /// mice were dumped and reported "did not respond to HID++".
    #[test]
    fn a_hidpp10_device_is_reported_instead_of_dumped() {
        let inventory = DeviceInventory {
            receiver: ReceiverInfo {
                name: "Unifying Receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc52b,
                unique_id: Some("receiver-uid".into()),
            },
            paired: vec![
                PairedDevice {
                    codename: Some("M510".into()),
                    hidpp10: true,
                    ..online(1)
                },
                PairedDevice {
                    codename: Some("K360".into()),
                    kind: DeviceKind::Keyboard,
                    ..online(2)
                },
                PairedDevice {
                    hidpp10: true,
                    ..online(3)
                },
                PairedDevice {
                    online: false,
                    ..online(4)
                },
            ],
        };

        assert_eq!(
            sections(&[inventory]),
            [
                Section {
                    name: "M510".into(),
                    route: slot(1),
                    read: Read::Hidpp10,
                },
                Section {
                    name: "K360".into(),
                    route: slot(2),
                    read: Read::Tables,
                },
                Section {
                    name: "Slot 3".into(),
                    route: slot(3),
                    read: Read::Hidpp10,
                },
            ]
        );
    }
}
