//! `openlogi unpair` — remove a device from a receiver's pairing table.
//!
//! Standard Logi Bolt / Unifying receiver operation: it only forgets the
//! slot, it never touches the physical device. A device removed this way
//! re-pairs normally afterwards.

use anyhow::{Context, Result};
use clap::Args;
use openlogi_hid::ReceiverSelector;

#[derive(Debug, Args)]
pub struct UnpairArgs {
    /// Bolt receiver unique ID to target, when more than one receiver is
    /// connected. Defaults to the first supported receiver found.
    #[arg(long, value_name = "UID")]
    pub receiver: Option<String>,

    /// Pairing slot to remove, as shown by `openlogi list`.
    #[arg(long)]
    pub slot: u8,
}

pub async fn run(args: UnpairArgs) -> Result<()> {
    let inventories = openlogi_hid::enumerate()
        .await
        .context("failed to enumerate HID++ devices")?;

    let inv = match &args.receiver {
        Some(uid) => inventories
            .iter()
            .find(|inv| inv.receiver.unique_id.as_deref() == Some(uid.as_str()))
            .with_context(|| format!("no connected receiver with UID {uid}"))?,
        None => inventories
            .first()
            .context("no connected Logitech HID++ receiver found")?,
    };

    let label = inv.paired.iter().find(|d| d.slot == args.slot).map_or_else(
        || format!("slot {}", args.slot),
        |d| {
            format!(
                "slot {} — {}",
                d.slot,
                d.codename.as_deref().unwrap_or("Unknown device")
            )
        },
    );
    println!("unpairing {label} from {} ...", inv.receiver.name);

    let selector = match &args.receiver {
        Some(uid) => ReceiverSelector::BoltUid(uid.clone()),
        None => ReceiverSelector::First,
    };
    openlogi_hid::unpair(selector, args.slot)
        .await
        .context("unpair failed")?;

    println!(
        "✓ unpaired — the receiver has forgotten this slot; re-pair the device normally to bring it back"
    );
    Ok(())
}
