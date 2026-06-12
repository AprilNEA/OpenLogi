//! `openlogi diag` — real-device smoke tests for the HID++ write path.
//!
//! Each subcommand exercises one round-trip (read → modify → read back →
//! restore). The intent is verification, not configuration: nothing here
//! touches `config.toml` or talks to the GUI; everything runs through the
//! same `openlogi_hid` API the GPUI app uses, so a green diag means the
//! GUI's write path works on this host.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use openlogi_hid::{DeviceRoute, dump_features};

pub mod dpi;
pub mod features;
pub mod lighting;
pub mod smartshift;

#[derive(Debug, Subcommand)]
pub enum DiagCmd {
    /// Dump every HID++ feature the active device reports.
    Features(features::FeaturesArgs),
    /// Read DPI → write a small delta → read back → restore → report.
    Dpi(dpi::DpiArgs),
    /// Read SmartShift mode → toggle → read back → toggle back → report.
    Smartshift(smartshift::SmartshiftArgs),
    /// Set a wired RGB keyboard to a solid colour (e.g. `ff0000` for red).
    Lighting(lighting::LightingArgs),
}

impl DiagCmd {
    pub async fn run(self) -> Result<()> {
        match self {
            Self::Features(args) => features::run(args).await,
            Self::Dpi(args) => dpi::run(args).await,
            Self::Smartshift(args) => smartshift::run(args).await,
            Self::Lighting(args) => lighting::run(args).await,
        }
    }
}

/// One online, paired device discovered during enumeration, already resolved to
/// the [`DeviceRoute`] needed to talk to it. Builds a Bolt route when the device
/// is behind a receiver, a direct route otherwise (USB cable / Bluetooth).
struct Candidate {
    route: DeviceRoute,
    name: String,
}

/// Enumerate inventories and resolve every *online* paired device to a route.
async fn online_devices() -> Result<Vec<Candidate>> {
    let inventories = openlogi_hid::enumerate().await?;
    let mut out = Vec::new();
    for inv in inventories {
        for paired in inv.paired.into_iter().filter(|p| p.online) {
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
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            out.push(Candidate { route, name });
        }
    }
    Ok(out)
}

/// Build a helpful "couldn't pick a device" error that lists what *is* online.
fn no_match_err(devices: &[Candidate], query: Option<&str>) -> anyhow::Error {
    if devices.is_empty() {
        return anyhow!("no online HID++ device found — is a Logi device paired and awake?");
    }
    let list = devices
        .iter()
        .map(|c| format!("    - {} ({})", c.name, c.route))
        .collect::<Vec<_>>()
        .join("\n");
    match query {
        Some(q) => anyhow!("no online device matches `--device {q}`.\n  online devices:\n{list}"),
        None => anyhow!(
            "could not pick a device automatically.\n  online devices:\n{list}\n  \
             pass --device <name> to choose one."
        ),
    }
}

/// Pick the device a diag should run against.
///
/// Selection order:
/// 1. If `query` is set, the first online device whose name contains it
///    (case-insensitive) — lets the user disambiguate explicitly.
/// 2. Else, if `required_features` is non-empty, the first online device whose
///    HID++ feature table exposes *any* of them. This is what stops a
///    mouse-only diag (DPI, SmartShift) from picking a paired keyboard when
///    several devices are online — a real hazard on Bluetooth-direct setups
///    where each device enumerates as its own inventory.
/// 3. Else, the first online device (the original behaviour).
pub(crate) async fn select_device(
    query: Option<&str>,
    required_features: &[u16],
) -> Result<(DeviceRoute, String)> {
    let devices = online_devices().await?;

    if let Some(q) = query {
        let needle = q.to_lowercase();
        return devices
            .iter()
            .find(|c| c.name.to_lowercase().contains(&needle))
            .map(|c| (c.route.clone(), c.name.clone()))
            .ok_or_else(|| no_match_err(&devices, query));
    }

    if !required_features.is_empty() {
        for c in &devices {
            match dump_features(&c.route).await {
                Ok(entries) => {
                    if entries.iter().any(|e| required_features.contains(&e.id)) {
                        return Ok((c.route.clone(), c.name.clone()));
                    }
                }
                Err(e) => {
                    // Sleepy/offline devices can fail legitimately; log so the
                    // silent fallthrough is visible if a healthy device is skipped.
                    tracing::warn!(
                        "skipping {} ({}): feature probe failed: {e:#}",
                        c.name,
                        c.route
                    );
                }
            }
        }
        // None advertised the feature — fall through to first-online so the
        // caller's own "device does not expose feature 0x….." error still
        // fires against a concrete device.
    }

    devices
        .into_iter()
        .next()
        .map(|c| (c.route, c.name))
        .ok_or_else(|| no_match_err(&[], None))
}
