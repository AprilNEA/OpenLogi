//! `openlogi diag controls` — dump HID++ reprogrammable controls, and with
//! `--reset` land every one of them on native reporting.

use std::fmt;

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::ReprogControlEntry;
use openlogi_hid::reprog_controls::CidReporting;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct ControlsArgs {
    /// Land every control on native reporting — temporary diversion and
    /// raw-XY off — before dumping the table. Recovers a device some process
    /// left captured (a killed or uninstalled agent, another Logitech client)
    /// whose pointer or buttons stopped responding. Quit the OpenLogi agent
    /// first: a live capture session would be undone underneath it.
    #[arg(long)]
    pub reset: bool,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting. Useful when several
    /// devices are paired (e.g. a mouse and a keyboard over Bluetooth).
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: ControlsArgs) -> Result<()> {
    // 0x1b04 = ReprogControlsV4 — the source of divertable HID++ button CIDs.
    let (route, name) = select_device(args.device.as_deref(), &[0x1b04]).await?;
    println!("device: {name} ({route})");

    if args.reset {
        let released = openlogi_hid::release_reprog_controls(&route)
            .await
            .context("release HID++ 0x1b04 controls to native reporting")?;
        if released.is_empty() {
            println!("  every control was already native");
        }
        for found in released {
            println!(
                "  released 0x{:04x} (was {})",
                found.control.cid,
                ReportingDisplay(found.reporting)
            );
        }
    }

    let controls = openlogi_hid::dump_reprog_reporting(&route)
        .await
        .context("dump HID++ 0x1b04 reprogrammable controls")?;
    if controls.is_empty() {
        println!("  no reprogrammable controls reported");
        return Ok(());
    }

    println!(
        "  {:>6}  {:>6}  {:>6}  {:<28}  capabilities",
        "cid", "task", "flags", "reporting"
    );
    for row in &controls {
        println!(
            "  0x{:04x}  0x{:04x}  0x{:04x}  {:<28}  {}",
            row.control.cid,
            row.control.task_id,
            row.control.flags.raw(),
            ReportingDisplay(row.reporting),
            ControlCapabilitiesDisplay(row.control)
        );
    }

    if args.reset {
        let still_captured: Vec<_> = controls
            .iter()
            .filter(|row| row.reporting.is_some_and(is_captured))
            .map(|row| format!("0x{:04x}", row.control.cid))
            .collect();
        if !still_captured.is_empty() {
            anyhow::bail!(
                "reset did not take: {} still report captured after the write",
                still_captured.join(", ")
            );
        }
        println!("✓ every readable control reports native");
    }
    Ok(())
}

/// Whether the control is in a state a capture session (or a dead one)
/// holds: diverted to software, or streaming raw XY.
fn is_captured(reporting: CidReporting) -> bool {
    reporting.diverted || reporting.raw_xy || reporting.force_raw_xy
}

/// The volatile reporting bits and remap of one control, or `?` when the
/// device would not read them back.
struct ReportingDisplay(Option<CidReporting>);

impl fmt::Display for ReportingDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(reporting) = self.0 else {
            return f.pad("?");
        };
        let mut parts: Vec<String> = [
            (reporting.diverted, "diverted"),
            (reporting.raw_xy, "raw-xy"),
            (reporting.force_raw_xy, "force-raw-xy"),
            (reporting.persistently_diverted, "persistent"),
        ]
        .into_iter()
        .filter(|&(enabled, _)| enabled)
        .map(|(_, name)| name.to_owned())
        .collect();
        if let Some(remap) = reporting.remap {
            parts.push(format!("remap→0x{:04x}", remap.0));
        }
        if parts.is_empty() {
            return f.pad("native");
        }
        // `pad`, not `write!`: the table column relies on the caller's width.
        f.pad(&parts.join(", "))
    }
}

struct ControlCapabilitiesDisplay(ReprogControlEntry);

impl fmt::Display for ControlCapabilitiesDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let flags = self.0.flags;
        let mut separator = "";
        for (enabled, name) in [
            (flags.is_divertable(), "divertable"),
            (flags.supports_raw_xy(), "raw-xy"),
            (flags.supports_force_raw_xy(), "force-raw-xy"),
            (flags.supports_analytics_key_events(), "analytics-events"),
            (flags.supports_raw_wheel(), "raw-wheel"),
        ] {
            if enabled {
                write!(f, "{separator}{name}")?;
                separator = ", ";
            }
        }
        if separator.is_empty() {
            write!(f, "-")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_hid::reprog_controls::{CidFlags, ControlId};

    fn reporting(diverted: bool, raw_xy: bool, force_raw_xy: bool) -> CidReporting {
        CidReporting {
            cid: ControlId(0x0053),
            diverted,
            persistently_diverted: false,
            force_raw_xy,
            raw_xy,
            remap: None,
            analytics_key_events: false,
            raw_wheel: false,
        }
    }

    #[test]
    fn summarizes_empty_capabilities() {
        let entry = ReprogControlEntry {
            cid: 0,
            task_id: 0,
            flags: CidFlags::default(),
        };

        assert_eq!(ControlCapabilitiesDisplay(entry).to_string(), "-");
    }

    #[test]
    fn summarizes_haptic_relevant_capabilities() {
        let entry = ReprogControlEntry {
            cid: 0,
            task_id: 0,
            flags: CidFlags::DIVERTABLE | CidFlags::FORCE_RAW_XY | CidFlags::ANALYTICS_KEY_EVENTS,
        };

        assert_eq!(
            ControlCapabilitiesDisplay(entry).to_string(),
            "divertable, force-raw-xy, analytics-events"
        );
    }

    #[test]
    fn unreadable_reporting_is_a_question_mark_not_native() {
        assert_eq!(ReportingDisplay(None).to_string(), "?");
    }

    #[test]
    fn reporting_honours_the_table_column_width() {
        // Only `Formatter::pad` respects the caller's width; a `write!`-based
        // impl would leave the capabilities column jagged.
        assert_eq!(format!("{:<8}|", ReportingDisplay(None)), "?       |");
        assert_eq!(
            format!(
                "{:<8}|",
                ReportingDisplay(Some(reporting(false, false, false)))
            ),
            "native  |"
        );
    }

    #[test]
    fn native_reporting_says_so() {
        let native = reporting(false, false, false);
        assert_eq!(ReportingDisplay(Some(native)).to_string(), "native");
        assert!(!is_captured(native));
    }

    #[test]
    fn a_stale_pointer_grabbing_diversion_names_every_set_bit() {
        let stuck = reporting(true, true, true);
        assert_eq!(
            ReportingDisplay(Some(stuck)).to_string(),
            "diverted, raw-xy, force-raw-xy"
        );
        assert!(is_captured(stuck));
    }

    #[test]
    fn a_remap_alone_is_not_captured_but_is_shown() {
        let remapped = CidReporting {
            remap: Some(ControlId(0x0056)),
            persistently_diverted: true,
            ..reporting(false, false, false)
        };
        assert_eq!(
            ReportingDisplay(Some(remapped)).to_string(),
            "persistent, remap→0x0056"
        );
        assert!(
            !is_captured(remapped),
            "persistent diversion and remap are the user's settings, not a leak"
        );
    }
}
