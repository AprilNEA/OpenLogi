//! `openlogi diag` — real-device smoke tests for the HID++ write path.
//!
//! Most subcommands exercise direct HID++ reads and verified writes. The
//! intent is diagnosis, not persistent configuration: nothing here touches
//! `config.toml` or talks to the GUI. They run through the same `openlogi_hid`
//! API the GPUI app uses, so a green diag means the GUI's write path works on
//! this host. `host-switch` instead goes through the running agent, because
//! the agent owns the device's HID channel and the device leaves this
//! computer's radio link after the write.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use openlogi_core::device::DeviceInventory;
use openlogi_hid::{DeviceRoute, dump_features};

pub mod battery;
pub mod controls;
pub mod dpi;
pub mod features;
pub mod host_switch;
pub mod lighting;
pub mod mouse_buttons;
pub mod onboard;
pub mod pointer_speed;
pub mod smartshift;
pub mod wheel;

#[derive(Debug, Subcommand)]
pub enum DiagCmd {
    /// Dump every HID++ feature the active device reports.
    Features(features::FeaturesArgs),
    /// Dump HID++ 0x1b04 reprogrammable controls and capability flags.
    Controls(controls::ControlsArgs),
    /// Read the raw battery report (0x1004, 0x1000, or 0x1F20 fields).
    Battery(battery::BatteryArgs),
    /// Read DPI → write a small delta → read back → restore → report.
    Dpi(dpi::DpiArgs),
    /// Read SmartShift mode → toggle → read back → toggle back → report.
    Smartshift(smartshift::SmartshiftArgs),
    /// Set an online RGB device to a solid colour (e.g. `ff0000` for red).
    Lighting(lighting::LightingArgs),
    /// Read or set the HID++ 0x2121 wheel reporting resolution.
    Wheel(wheel::WheelArgs),
    /// Read 0x2205 pointer scaling → write a test value → read back → restore.
    PointerSpeed(pointer_speed::PointerSpeedArgs),
    /// Read a G-series mouse's onboard profiles and mode.
    Onboard(onboard::OnboardArgs),
    /// Print which HID++ 0x8110 slot each button of a G-series mouse sends.
    MouseButtons(mouse_buttons::MouseButtonsArgs),
    /// Switch a multi-host device through the running agent (HID++ 0x1814).
    HostSwitch(host_switch::HostSwitchArgs),
}

impl DiagCmd {
    pub async fn run(self) -> Result<()> {
        #[cfg(target_os = "macos")]
        if self.opens_devices_directly() {
            super::input_monitoring::request().await?;
        }

        match self {
            Self::Features(args) => features::run(args).await,
            Self::Controls(args) => controls::run(args).await,
            Self::Battery(args) => battery::run(args).await,
            Self::Dpi(args) => dpi::run(args).await,
            Self::Smartshift(args) => smartshift::run(args).await,
            Self::Lighting(args) => lighting::run(args).await,
            Self::Wheel(args) => wheel::run(args).await,
            Self::PointerSpeed(args) => pointer_speed::run(args).await,
            Self::Onboard(args) => onboard::run(args).await,
            Self::MouseButtons(args) => mouse_buttons::run(args).await,
            Self::HostSwitch(args) => host_switch::run(args).await,
        }
    }

    /// Whether this subcommand opens a device itself. `host-switch` writes
    /// through the agent's channel, so it needs no Input Monitoring grant of
    /// its own.
    #[cfg(target_os = "macos")]
    fn opens_devices_directly(&self) -> bool {
        !matches!(self, Self::HostSwitch(_))
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
    for inventory in &inventories {
        out.extend(online_candidates(inventory));
    }
    Ok(out)
}

fn online_candidates(inventory: &DeviceInventory) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for paired in inventory.paired.iter().filter(|paired| paired.online) {
        if let Some(route) = DeviceRoute::for_slot(inventory, paired.slot) {
            let name = paired
                .codename
                .clone()
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            candidates.push(Candidate { route, name });
        } else {
            tracing::warn!(
                receiver = %inventory.receiver.name,
                slot = paired.slot,
                "skipping online device without a resolvable route"
            );
        }
    }
    candidates
}

/// The one device in `devices` whose name contains `query`,
/// case-insensitively.
///
/// When several names contain it, the one whose whole name equals `query`
/// wins, so "MX Master 3S" still picks that mouse beside an "MX Master 3S for
/// Mac". Any other ambiguity is an error that lists the matches: a diag that
/// writes to a device must not pick one of them by enumeration order. So is a
/// chosen route that another online device shares: two Bluetooth-direct
/// devices with one vendor/product id have one route, whatever their names.
fn select_named_device(devices: &[Candidate], query: &str) -> Result<(DeviceRoute, String)> {
    let needle = query.to_lowercase();
    let matches: Vec<&Candidate> = devices
        .iter()
        .filter(|c| c.name.to_lowercase().contains(&needle))
        .collect();
    let chosen = match matches.as_slice() {
        [] => return Err(no_match_err(devices, Some(query))),
        [only] => *only,
        several => {
            let mut exact = several.iter().filter(|c| c.name.to_lowercase() == needle);
            match (exact.next(), exact.next()) {
                (Some(only), None) => *only,
                _ => return Err(ambiguous_err(several, query)),
            }
        }
    };
    let sharing: Vec<&Candidate> = devices.iter().filter(|c| c.route == chosen.route).collect();
    if sharing.len() > 1 {
        let list = candidate_list(&sharing);
        return Err(anyhow!(
            "several online devices share the route {}:\n{list}\n  \
             it cannot address `{}` alone; no operation was attempted.",
            chosen.route,
            chosen.name
        ));
    }
    Ok((chosen.route.clone(), chosen.name.clone()))
}

fn candidate_list(candidates: &[&Candidate]) -> String {
    candidates
        .iter()
        .map(|c| format!("    - {} ({})", c.name, c.route))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ambiguous_err(matches: &[&Candidate], query: &str) -> anyhow::Error {
    let list = candidate_list(matches);
    anyhow!(
        "several online devices match `--device {query}`:\n{list}\n  \
         pass a longer or exact name to choose one."
    )
}

/// Pick an online device by name from an inventory the agent reported, for
/// subcommands that write through the agent instead of opening a device.
pub(crate) fn select_inventory_device(
    inventories: &[DeviceInventory],
    query: &str,
) -> Result<(DeviceRoute, String)> {
    let devices: Vec<Candidate> = inventories.iter().flat_map(online_candidates).collect();
    select_named_device(&devices, query)
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
/// 1. If `query` is set, the one online device whose name contains it
///    (case-insensitive), as [`select_named_device`] decides — ambiguous
///    matches are refused.
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
        return select_named_device(&devices, q);
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

#[cfg(test)]
mod tests {
    use openlogi_core::device::{
        Capabilities, DeviceInventory, DeviceKind, PairedDevice, ReceiverInfo,
    };
    use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};

    use super::{
        Candidate, no_match_err, online_candidates, select_inventory_device, select_named_device,
    };

    fn candidate(name: &str, product_id: u16) -> Candidate {
        Candidate {
            route: DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id,
            },
            name: name.to_string(),
        }
    }

    #[test]
    fn no_devices_at_all_gives_a_generic_not_found_message() {
        let err = no_match_err(&[], None).to_string();
        assert_eq!(
            err,
            "no online HID++ device found — is a Logi device paired and awake?"
        );
    }

    #[test]
    fn no_devices_at_all_ignores_a_query_and_still_uses_the_generic_message() {
        // `devices.is_empty()` short-circuits before `query` is inspected.
        let err = no_match_err(&[], Some("mouse")).to_string();
        assert_eq!(
            err,
            "no online HID++ device found — is a Logi device paired and awake?"
        );
    }

    #[test]
    fn unmatched_query_names_the_query_and_lists_online_devices() {
        let devices = vec![
            candidate("MX Master 3S", 0xb034),
            candidate("G Pro", 0xc094),
        ];
        let err = no_match_err(&devices, Some("keyboard")).to_string();

        assert!(err.contains("no online device matches `--device keyboard`"));
        assert!(err.contains("MX Master 3S"));
        assert!(err.contains("G Pro"));
    }

    #[test]
    fn no_query_suggests_the_device_flag_and_lists_online_devices() {
        let devices = vec![candidate("MX Master 3S", 0xb034)];
        let err = no_match_err(&devices, None).to_string();

        assert!(err.contains("could not pick a device automatically"));
        assert!(err.contains("pass --device <name> to choose one"));
        assert!(err.contains("MX Master 3S"));
    }

    #[test]
    fn ambiguous_query_is_refused_and_lists_the_matches() {
        let devices = vec![
            candidate("MX Master 3S", 0xb034),
            candidate("MX Mechanical", 0xb366),
        ];

        let err = select_named_device(&devices, "mx")
            .expect_err("two substring matches must be refused")
            .to_string();

        assert!(err.contains("several online devices match `--device mx`"));
        assert!(err.contains("MX Master 3S"));
        assert!(err.contains("MX Mechanical"));
    }

    #[test]
    fn a_unique_exact_name_wins_over_longer_matches() {
        let devices = vec![
            candidate("MX Master 3S for Mac", 0xb043),
            candidate("MX Master 3S", 0xb034),
        ];

        let (_, name) =
            select_named_device(&devices, "mx master 3s").expect("the exact name is chosen");

        assert_eq!(name, "MX Master 3S");
    }

    #[test]
    fn identical_names_stay_ambiguous() {
        let devices = vec![
            candidate("MX Master 3S", 0xb034),
            candidate("MX Master 3S", 0xb043),
        ];

        select_named_device(&devices, "MX Master 3S")
            .expect_err("two devices with one name must be refused");
    }

    #[test]
    fn a_unique_name_on_a_shared_route_is_refused() {
        // Two Bluetooth-direct devices with one vendor/product id share a
        // route, so the agent could act on either of them.
        let devices = vec![
            candidate("MX Master 3S", 0xb034),
            candidate("MX Master 3S (work)", 0xb034),
            candidate("G Pro", 0xc094),
        ];

        let err = select_named_device(&devices, "(work)")
            .expect_err("a route two devices share must be refused")
            .to_string();

        assert!(err.contains("several online devices share the route"));
        assert!(err.contains("MX Master 3S (work)"));
        assert!(!err.contains("G Pro"));
        let (_, name) = select_named_device(&devices, "g pro").expect("a route of its own");
        assert_eq!(name, "G Pro");
    }

    #[test]
    fn unresolved_receiver_route_is_ignored() {
        let inventory = inventory(0xc547, None, vec![paired(2, "G502 X Plus", true)]);

        assert!(online_candidates(&inventory).is_empty());
    }

    #[test]
    fn receiver_route_retains_uid_and_slot() {
        let inventory = inventory(
            0xc547,
            Some("receiver-uid"),
            vec![paired(4, "G502 X Plus", true)],
        );
        let candidates = online_candidates(&inventory);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "G502 X Plus");
        assert_eq!(
            candidates[0].route,
            DeviceRoute::Unifying {
                receiver_uid: "receiver-uid".into(),
                slot: 4,
            }
        );
    }

    #[test]
    fn direct_route_retains_direct_device_index() {
        let inventory = inventory(
            0xc095,
            None,
            vec![paired(DIRECT_DEVICE_INDEX, "G502 X Plus", true)],
        );
        let candidates = online_candidates(&inventory);

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].route,
            DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc095,
            }
        );
        assert_eq!(candidates[0].route.device_index(), DIRECT_DEVICE_INDEX);
    }

    #[test]
    fn offline_device_is_ignored() {
        let inventory = inventory(
            0xc547,
            Some("receiver-uid"),
            vec![paired(2, "G502 X Plus", false)],
        );

        assert!(online_candidates(&inventory).is_empty());
    }

    #[test]
    fn agent_inventory_selection_skips_offline_devices() {
        let inventories = [inventory(
            0xc547,
            Some("receiver-uid"),
            vec![
                paired(1, "MX Master 3S", false),
                paired(2, "MX Master 3S", true),
            ],
        )];

        let (route, name) =
            select_inventory_device(&inventories, "master").expect("the online device matches");

        assert_eq!(name, "MX Master 3S");
        assert_eq!(
            route,
            DeviceRoute::Unifying {
                receiver_uid: "receiver-uid".into(),
                slot: 2,
            }
        );
    }

    fn inventory(
        product_id: u16,
        unique_id: Option<&str>,
        paired: Vec<PairedDevice>,
    ) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Test device".into(),
                vendor_id: 0x046d,
                product_id,
                unique_id: unique_id.map(str::to_string),
            },
            paired,
        }
    }

    fn paired(slot: u8, name: &str, online: bool) -> PairedDevice {
        PairedDevice {
            slot,
            codename: Some(name.into()),
            wpid: None,
            kind: DeviceKind::Mouse,
            online,
            battery: None,
            model_info: None,
            capabilities: Some(Capabilities::default()),
            settings_revision: 0,
            hidpp10: false,
        }
    }
}
