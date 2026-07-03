//! `openlogi diag controls` — dump HID++ reprogrammable controls.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::ReprogControlEntry;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct ControlsArgs {
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

    let controls = openlogi_hid::dump_reprog_controls(&route)
        .await
        .context("dump HID++ 0x1b04 reprogrammable controls")?;
    if controls.is_empty() {
        println!("  no reprogrammable controls reported");
        return Ok(());
    }

    println!(
        "  {:>6}  {:>6}  {:>6}  capabilities",
        "cid", "task", "flags"
    );
    for control in controls {
        println!(
            "  0x{:04x}  0x{:04x}  0x{:04x}  {}",
            control.cid,
            control.task_id,
            control.flags.raw(),
            summarize_capabilities(control)
        );
    }
    Ok(())
}

fn summarize_capabilities(control: ReprogControlEntry) -> String {
    let mut caps = Vec::new();
    if control.flags.is_divertable() {
        caps.push("divertable");
    }
    if control.flags.supports_raw_xy() {
        caps.push("raw-xy");
    }
    if control.flags.supports_force_raw_xy() {
        caps.push("force-raw-xy");
    }
    if control.flags.supports_analytics_key_events() {
        caps.push("analytics-events");
    }
    if control.flags.supports_raw_wheel() {
        caps.push("raw-wheel");
    }
    if caps.is_empty() {
        "-".to_string()
    } else {
        caps.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_empty_capabilities() {
        let entry = ReprogControlEntry {
            cid: 0,
            task_id: 0,
            flags: openlogi_hid::reprog_controls::CidFlags::default(),
        };

        assert_eq!(summarize_capabilities(entry), "-");
    }

    #[test]
    fn summarizes_haptic_relevant_capabilities() {
        let entry = ReprogControlEntry {
            cid: 0,
            task_id: 0,
            flags: openlogi_hid::reprog_controls::CidFlags::DIVERTABLE
                | openlogi_hid::reprog_controls::CidFlags::FORCE_RAW_XY
                | openlogi_hid::reprog_controls::CidFlags::ANALYTICS_KEY_EVENTS,
        };

        assert_eq!(
            summarize_capabilities(entry),
            "divertable, force-raw-xy, analytics-events"
        );
    }
}
