//! `openlogi diag features` — dump the device's HID++ feature table.
//!
//! Useful for figuring out *which* DPI / SmartShift / etc. feature ID a
//! given peripheral exposes when the default wrappers (0x2201, 0x2111)
//! aren't recognised.

use std::fmt;

use anyhow::Result;
use clap::Args;
use openlogi_hid::{DeviceRoute, FeatureType, FirmwareEntity};

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
            match paired.codename.as_deref() {
                Some(name) => println!("device: {name} ({route})"),
                None => println!("device: Slot {} ({route})", paired.slot),
            }
            match openlogi_hid::dump_features(&route).await {
                Ok(entries) => {
                    println!("  {:>4}  {:>6}  {:<4}  flags", "idx", "id", "ver");
                    for (idx, entry) in entries.iter().enumerate() {
                        println!(
                            "  {:>4}  0x{:04x}  v{:<3}  {}",
                            idx,
                            entry.id,
                            entry.version,
                            FeatureFlagsDisplay(entry.typ)
                        );
                    }
                    println!("  ({} feature entries)\n", entries.len());
                }
                Err(e) => println!("  dump failed: {e:#}\n"),
            }
            match openlogi_hid::dump_firmware_entities(&route).await {
                Ok(entries) => {
                    for entry in &entries {
                        println!("  {}", FirmwareEntityDisplay(entry));
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
                    "fw {index}: {:?} {}{:02}.{:02}_B{:04} pid={:04x}",
                    info.kind,
                    info.prefix,
                    info.number,
                    info.revision,
                    info.build,
                    info.transport_pid
                )?;
                if info.active {
                    write!(f, " [active]")?;
                }

                // `extra_version` is optional by spec, and all-zero is how a
                // device says it has none — so an empty field is absence, not
                // a value being hidden. The PID above is not optional and is
                // printed even when the device answers zero, which only a
                // dormant entity is allowed to do.
                if info.extra_version != [0; 5] {
                    write!(f, " extra=")?;
                    for byte in info.extra_version {
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
    use openlogi_hid::{
        DeviceEntityType, FeatureType, FirmwareEntity, FirmwareEntityInfo, HidppOperation,
        WriteError,
    };

    use super::{FeatureFlagsDisplay, FirmwareEntityDisplay};

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
                transport_pid: 0xc08d,
                extra_version: [0; 5],
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
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 1: MainApplication MPM17.00_B0008 pid=c08d"
        );
    }

    /// Only the active entity has to report a real PID, and a dormant one may
    /// answer zero. That is still what the device said, so it is printed.
    #[test]
    fn a_zero_pid_is_reported_rather_than_hidden() {
        let mut e = entry();
        info(&mut e).active = false;
        info(&mut e).transport_pid = 0;
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 1: MainApplication MPM17.00_B0008 pid=0000"
        );
    }

    /// `extra_version` is device-specific and normally zero. When a device
    /// does fill it in, the bytes are the whole reason the field is carried.
    #[test]
    fn populated_extra_version_bytes_are_shown() {
        let mut e = entry();
        info(&mut e).extra_version = [0x01, 0x02, 0x00, 0xff, 0x10];
        assert_eq!(
            FirmwareEntityDisplay(&e).to_string(),
            "fw 1: MainApplication MPM17.00_B0008 pid=c08d [active] extra=010200ff10"
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
}
