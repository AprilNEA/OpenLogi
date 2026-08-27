//! `openlogi diag hosts` — read the Easy-Switch host table.

use std::fmt;

use anyhow::Result;
use clap::Args;
use openlogi_hid::{DiagnosticHostBus, DiagnosticHostSlotStatus};

use super::select_device;

#[derive(Debug, Args)]
pub struct HostsArgs {
    /// Device name (or a unique substring) to inspect.
    #[arg(long)]
    device: Option<String>,
}

pub async fn run(args: HostsArgs) -> Result<()> {
    let (route, name) = select_device(args.device.as_deref(), &[0x1815]).await?;
    let hosts = openlogi_hid::dump_hosts(&route).await?;

    println!("device: {name} ({route})");
    match hosts.current_host {
        Some(slot) => println!("current host: {}", slot + 1),
        None => println!("current host: unknown"),
    }
    for slot in &hosts.slots {
        let current = if hosts.current_host == Some(slot.index) {
            " [current]"
        } else {
            ""
        };
        println!(
            "  host {}: {}, {}{}",
            slot.index + 1,
            StatusDisplay(slot.status),
            BusDisplay(slot.bus),
            current
        );
    }
    Ok(())
}

struct StatusDisplay(DiagnosticHostSlotStatus);

impl fmt::Display for StatusDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            DiagnosticHostSlotStatus::Empty => "empty",
            DiagnosticHostSlotStatus::Paired => "paired",
        })
    }
}

struct BusDisplay(DiagnosticHostBus);

impl fmt::Display for BusDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            DiagnosticHostBus::Undefined => "unknown transport",
            DiagnosticHostBus::Equad => "eQuad",
            DiagnosticHostBus::Usb => "USB",
            DiagnosticHostBus::Bluetooth => "Bluetooth",
            DiagnosticHostBus::BluetoothLowEnergy => "Bluetooth LE",
            DiagnosticHostBus::Bolt => "Logi Bolt",
        })
    }
}

#[cfg(test)]
mod tests {
    use openlogi_hid::{DiagnosticHostBus, DiagnosticHostSlotStatus};

    use super::{BusDisplay, StatusDisplay};

    #[test]
    fn statuses_are_plain_language() {
        assert_eq!(
            StatusDisplay(DiagnosticHostSlotStatus::Paired).to_string(),
            "paired"
        );
        assert_eq!(
            StatusDisplay(DiagnosticHostSlotStatus::Empty).to_string(),
            "empty"
        );
    }

    #[test]
    fn bolt_is_named_for_the_user() {
        assert_eq!(BusDisplay(DiagnosticHostBus::Bolt).to_string(), "Logi Bolt");
    }
}
