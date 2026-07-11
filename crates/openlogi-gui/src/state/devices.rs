//! Device-list construction and selection helpers for [`super::AppState`].

use std::collections::HashSet;

use openlogi_agent_core::device_order::DeviceStableId;
use openlogi_core::config::{Config, DeviceIdentity};
use openlogi_core::device::{
    BatteryInfo, Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
};
use openlogi_hid::DeviceRoute;
use tracing::debug;

use crate::asset::{AssetResolver, ResolvedAsset};

/// One paired device with everything the UI needs to switch to it in O(1):
/// the physical config key (for bindings/DPI persistence), a display name, the
/// resolved asset (PNG + metadata, or `None` for the synthetic fallback),
/// and the [`DeviceRoute`] HID++ writes / capture target.
///
/// The `kind` / `slot` / `online` / `battery` fields mirror the source
/// [`PairedDevice`](openlogi_core::device::PairedDevice) so the header
/// carousel can render straight from the device list — the list is the single
/// source of truth for "which devices exist", keeping carousel order aligned
/// with [`super::AppState::current_device`].
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    /// Stable physical-device key used for persisted settings.
    pub config_key: String,
    /// Stable model key used only for asset/model lookup and diagnostics.
    pub model_key: String,
    pub display_name: String,
    pub asset: Option<ResolvedAsset>,
    pub model_info: Option<DeviceModelInfo>,
    pub codename: Option<String>,
    pub serial_number: Option<String>,
    pub unit_id: [u8; 4],
    pub route: Option<DeviceRoute>,
    pub kind: DeviceKind,
    /// Configuration capabilities from the device's HID++ feature table.
    /// Continuity across sleep lives in the hid layer: its probe cache keeps
    /// serving the last-known capabilities for a known-but-offline device, so
    /// this is `None` only for a device never probed since the agent started —
    /// and the UI then falls back to [`Capabilities::presumed_from_kind`].
    pub capabilities: Option<Capabilities>,
    pub slot: u8,
    pub online: bool,
    pub battery: Option<BatteryInfo>,
}

/// Build the carousel's device list as the **union** of the live inventory and
/// the persisted set of devices we've seen before.
///
/// Live devices come from `inventories` (the agent's current HID++ probe).
/// Every device the user has previously seen online but that is *absent* from
/// this snapshot — asleep, or not yet re-probed after a cold start — is added
/// back as an offline placeholder from [`Config::known_identities`]. This is
/// what makes the list independent of whether a probe wins its timing race: a
/// known device (with its Pointer/Buttons panels) is always shown, and the live
/// probe only *enriches* it (online state, battery, asset photo) rather than
/// *gating* whether it appears at all. See issue #159.
pub(super) fn build_device_list(
    inventories: &[DeviceInventory],
    cache: &AssetResolver,
    config: &Config,
) -> Vec<DeviceRecord> {
    let mut list = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let route = DeviceRoute::device_route_for(inv, paired.slot);
            let (model_key, asset, model_info, codename, serial_number, unit_id) =
                if let Some(model) = paired.model_info.as_ref() {
                    let asset = cache.resolve(model, paired.codename.as_deref());
                    (
                        model.config_key(),
                        asset,
                        Some(model.clone()),
                        paired.codename.clone(),
                        model.serial_number.clone(),
                        model.unit_id,
                    )
                } else {
                    // No HID++ 2.0 model info — HID++ 1.0 device or feature walk
                    // timed out. Surface the device anyway using the wpid (or slot
                    // as a last-resort model key) so it appears in the carousel
                    // with a stable display fallback.
                    let key = paired.wpid.map_or_else(
                        || format!("slot{}", paired.slot),
                        |w| format!("wpid{w:04x}"),
                    );
                    (key, None, None, paired.codename.clone(), None, [0u8; 4])
                };
            let config_key = DeviceStableId::from_parts(
                route.as_ref(),
                paired.slot,
                serial_number.as_deref(),
                unit_id,
            )
            .config_key();

            let display_name = asset
                .as_ref()
                .map(|a| a.display_name.clone())
                .or_else(|| paired.codename.as_deref().map(prettify_codename))
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            let kind = effective_kind(paired.kind, asset.as_ref().map(|a| a.kind));
            list.push(DeviceRecord {
                config_key,
                model_key,
                display_name,
                asset,
                model_info,
                codename,
                serial_number,
                unit_id,
                route,
                kind,
                capabilities: paired.capabilities,
                slot: paired.slot,
                online: paired.online,
                battery: paired.battery.clone(),
            });
        }
    }
    #[cfg(debug_assertions)]
    if std::env::var_os("OPENLOGI_DEMO_KEYBOARD").is_some() {
        list.push(demo_keyboard());
    }
    append_offline_known(&mut list, config.known_identities(), cache);
    sort_device_list(&mut list);
    list
}

/// Append an offline placeholder for every known device not already present in
/// `list` (matched by `config_key`). Split out from [`build_device_list`] so
/// the union rule is unit-testable without an [`AssetResolver`].
fn append_offline_known<'a>(
    list: &mut Vec<DeviceRecord>,
    known: impl Iterator<Item = (&'a str, &'a DeviceIdentity)>,
    cache: &AssetResolver,
) {
    let mut blocked_keys: HashSet<String> = list
        .iter()
        .flat_map(|r| [r.config_key.clone(), r.model_key.clone()])
        .collect();
    let mut known = known.collect::<Vec<_>>();
    known.sort_by_key(|(key, identity)| (identity.model_info.is_none(), (*key).to_string()));

    for (key, identity) in known {
        let model_key = identity
            .model_info
            .as_ref()
            .map_or_else(|| key.to_string(), DeviceModelInfo::config_key);
        if blocked_keys.contains(key) || blocked_keys.contains(&model_key) {
            continue;
        }
        let record = offline_record(key, identity, cache);
        blocked_keys.insert(record.config_key.clone());
        blocked_keys.insert(record.model_key.clone());
        list.push(record);
    }
}

/// Synthesize an offline placeholder from a persisted [`DeviceIdentity`].
///
/// `route: None` keeps every hardware write a no-op until the live inventory
/// supplies the real route when the device wakes; `capabilities: Some(..)` from
/// the persisted measurement is what keeps the device's config panels visible
/// while it sleeps. When the identity was written by a version that persisted
/// model info, the cached asset is resolved immediately so cold-start cards do
/// not flash the synthetic silhouette while waiting for live inventory.
fn offline_record(
    config_key: &str,
    identity: &DeviceIdentity,
    cache: &AssetResolver,
) -> DeviceRecord {
    let model_info = identity
        .model_info
        .clone()
        .or_else(|| model_info_from_legacy_model_key(config_key));
    let asset = model_info
        .as_ref()
        .and_then(|model| cache.resolve(model, identity.codename.as_deref()));
    let model_key = model_info
        .as_ref()
        .map_or_else(|| config_key.to_string(), DeviceModelInfo::config_key);
    DeviceRecord {
        config_key: config_key.to_string(),
        model_key,
        display_name: identity.display_name.clone(),
        asset,
        model_info,
        codename: identity.codename.clone(),
        serial_number: None,
        unit_id: [0; 4],
        route: None,
        kind: identity.kind,
        capabilities: Some(identity.capabilities),
        slot: 0,
        online: false,
        battery: None,
    }
}

fn model_info_from_legacy_model_key(key: &str) -> Option<DeviceModelInfo> {
    if key.len() <= 4 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let split = key.len() - 4;
    let (ext, pid) = key.split_at(split);
    Some(DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports::default(),
        model_ids: [u16::from_str_radix(pid, 16).ok()?, 0, 0],
        extended_model_id: u8::from_str_radix(ext, 16).ok()?,
    })
}

/// Order the carousel by physical route. HID enumeration order can change as
/// different mice wake, sleep, or are selected; sorting by the stable route
/// (not whichever HID node was reported first) keeps the header stable.
/// Applied both on a fresh build and after [`super::AppState`] merges a
/// snapshot, so a newly-appeared device lands in its canonical slot rather than
/// being appended.
pub(super) fn sort_device_list(list: &mut [DeviceRecord]) {
    list.sort_by_key(device_order_key);
}

fn device_order_key(record: &DeviceRecord) -> (DeviceStableId, String, String) {
    (
        DeviceStableId::from_parts(
            record.route.as_ref(),
            record.slot,
            record.serial_number.as_deref(),
            record.unit_id,
        ),
        record.model_key.clone(),
        record.display_name.clone(),
    )
}

/// Dev-only synthetic keyboard so the keyboard detail panel + lighting controls
/// render without the hardware. Gated behind the `OPENLOGI_DEMO_KEYBOARD` env
/// var (debug builds only); `route: None` keeps every hardware write a no-op.
#[cfg(debug_assertions)]
fn demo_keyboard() -> DeviceRecord {
    DeviceRecord {
        config_key: "demo-g513".to_string(),
        model_key: "demo-g513".to_string(),
        display_name: "Logitech G513".to_string(),
        asset: None,
        model_info: None,
        codename: None,
        serial_number: None,
        unit_id: [0; 4],
        route: None,
        kind: DeviceKind::Keyboard,
        capabilities: Some(Capabilities {
            lighting: true,
            ..Capabilities::default()
        }),
        slot: 0,
        online: true,
        battery: None,
    }
}

/// Last step of the device-kind precedence chain:
///
/// > **asset registry** > HID++ `0x0005` > Bolt pairing register
///
/// The two HID++ sources are already folded into `hid_kind` by
/// `resolve_device_kind` (`crates/openlogi-hid/src/inventory.rs`); this applies
/// the final override. Adding a kind source means slotting it into this one
/// chain — here if it should beat the HID++ sources, in `resolve_device_kind`
/// otherwise — and updating both docs.
///
/// The registry type wins because it is per-model and human-maintained, so a
/// device that matched a known depot is classified by what that model *is* —
/// not by a Bolt pairing register that can misreport (the failure behind #127).
/// We fall back to `hid_kind` when there is no asset or its type is `Unknown`.
/// A genuine disagreement is logged at debug (the list rebuilds on every
/// snapshot, so a louder level would spam); it flags a HID++ source we
/// shouldn't trust for that device.
///
/// Kind is cosmetic (icon / label) since #127: config panels gate on
/// [`Capabilities`], never on kind, so a wrong pick can't hide functionality.
fn effective_kind(hid_kind: DeviceKind, asset_kind: Option<DeviceKind>) -> DeviceKind {
    let Some(asset_kind) = asset_kind.filter(|k| *k != DeviceKind::Unknown) else {
        return hid_kind;
    };
    if hid_kind != DeviceKind::Unknown && hid_kind != asset_kind {
        debug!(
            ?hid_kind,
            ?asset_kind,
            "HID++ device kind disagrees with the asset registry — trusting the registry"
        );
    }
    asset_kind
}

pub(super) fn pick_initial_device(list: &[DeviceRecord], saved: Option<&str>) -> usize {
    saved
        .and_then(|key| list.iter().position(|r| r.config_key == key))
        .unwrap_or(0)
}

/// Tidy a raw HID++ codename for display when no curated asset name exists.
/// Logitech reports gaming codenames in ALL CAPS (e.g. `"G513 RGB MECHANICAL
/// GAMING KEYBOARD"`); title-case each word so it reads like the asset names
/// (`"MX Master 3S"`) instead of shouting, while keeping model numbers (tokens
/// with a digit, e.g. `G513`) and short acronyms (`RGB`, `TKL`, `SE`) as-is.
/// Codenames already in mixed case are returned unchanged.
fn prettify_codename(raw: &str) -> String {
    if raw.chars().any(char::is_lowercase) {
        return raw.to_string();
    }
    raw.split_whitespace()
        .map(|word| {
            if word.len() <= 3 || word.bytes().any(|b| b.is_ascii_digit()) {
                word.to_string()
            } else {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use openlogi_core::config::Config;
    use openlogi_core::device::{DeviceInventory, PairedDevice, ReceiverInfo};

    use crate::asset::AssetResolver;

    use super::{
        Capabilities, DeviceIdentity, DeviceKind, DeviceRecord, append_offline_known,
        build_device_list, effective_kind, offline_record,
    };

    fn paired_device_no_model_info(slot: u8, wpid: Option<u16>) -> PairedDevice {
        PairedDevice {
            slot,
            codename: None,
            wpid,
            kind: DeviceKind::Keyboard,
            online: true,
            battery: None,
            model_info: None,
            capabilities: None,
        }
    }

    fn inventory_with(devices: Vec<PairedDevice>) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Unifying Receiver".into(),
                vendor_id: 0x046d,
                product_id: 0xc52b,
                unique_id: Some("DA2699E1".into()),
            },
            paired: devices,
        }
    }

    fn online_record(key: &str) -> DeviceRecord {
        DeviceRecord {
            config_key: key.to_string(),
            model_key: key.to_string(),
            display_name: format!("live {key}"),
            asset: None,
            model_info: None,
            codename: None,
            serial_number: None,
            unit_id: [1; 4],
            route: None,
            kind: DeviceKind::Mouse,
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
            slot: 1,
            online: true,
            battery: None,
        }
    }

    fn mouse_identity(name: &str) -> DeviceIdentity {
        DeviceIdentity {
            display_name: name.to_string(),
            kind: DeviceKind::Mouse,
            capabilities: Capabilities {
                buttons: true,
                pointer: true,
                lighting: false,
                scroll_inversion: false,
                thumbwheel_tap: false,
            },
            model_info: None,
            codename: None,
        }
    }

    #[test]
    fn no_model_info_uses_receiver_slot_as_config_key() {
        let inv = inventory_with(vec![paired_device_no_model_info(1, Some(0x4076))]);
        let cache = AssetResolver::new();
        let list = build_device_list(&[inv], &cache, &Config::default());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].config_key, "receiver:da2699e1:slot:1");
        assert_eq!(list[0].model_key, "wpid4076");
        assert!(list[0].serial_number.is_none());
        assert_eq!(list[0].unit_id, [0u8; 4]);
    }

    #[test]
    fn no_model_info_falls_back_to_slot_when_no_wpid() {
        let inv = inventory_with(vec![paired_device_no_model_info(3, None)]);
        let cache = AssetResolver::new();
        let list = build_device_list(&[inv], &cache, &Config::default());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].config_key, "receiver:da2699e1:slot:3");
        assert_eq!(list[0].model_key, "slot3");
    }

    #[test]
    fn no_model_info_display_name_falls_back_to_slot() {
        let inv = inventory_with(vec![paired_device_no_model_info(2, Some(0x4051))]);
        let cache = AssetResolver::new();
        let list = build_device_list(&[inv], &cache, &Config::default());
        assert_eq!(list[0].display_name, "Slot 2");
    }

    #[test]
    fn offline_record_is_present_but_inert() {
        // A persisted identity renders as an offline card that still carries its
        // measured capabilities (so its panels show) but no route (so writes are
        // no-ops until it wakes).
        let id = mouse_identity("MX Master 3S");
        let cache = AssetResolver::new();
        let rec = offline_record("2b034", &id, &cache);
        assert_eq!(rec.config_key, "2b034");
        assert_eq!(rec.display_name, "MX Master 3S");
        assert!(!rec.online);
        assert!(rec.route.is_none());
        assert_eq!(rec.capabilities, Some(id.capabilities));
    }

    #[test]
    fn known_devices_are_appended_only_when_absent_from_live() {
        // "A" is live; "B" is known-but-asleep. The union keeps the live "A"
        // untouched and adds "B" back as an offline placeholder — the core of
        // the #159 fix: a sleeping device never drops out of the list.
        let mut list = vec![online_record("A")];
        let a = mouse_identity("live A overwritten?");
        let b = mouse_identity("asleep B");
        let cache = AssetResolver::new();
        append_offline_known(&mut list, [("A", &a), ("B", &b)].into_iter(), &cache);

        assert_eq!(list.len(), 2);
        assert!(
            list.iter().any(|r| r.config_key == "A" && r.online),
            "the live record for A must win over its identity"
        );
        assert!(
            list.iter().any(|r| r.config_key == "B" && !r.online),
            "B is added back as a persisted offline placeholder"
        );
    }

    #[test]
    fn asset_kind_overrides_a_misreporting_hid_kind() {
        // #127: the registry knows this depot is a mouse, so a HID++ source that
        // reported `Keyboard` loses.
        assert_eq!(
            effective_kind(DeviceKind::Keyboard, Some(DeviceKind::Mouse)),
            DeviceKind::Mouse
        );
    }

    #[test]
    fn hid_kind_is_used_without_a_modelled_asset() {
        // No asset, or an asset whose type we don't model → keep the HID kind.
        assert_eq!(effective_kind(DeviceKind::Mouse, None), DeviceKind::Mouse);
        assert_eq!(
            effective_kind(DeviceKind::Mouse, Some(DeviceKind::Unknown)),
            DeviceKind::Mouse
        );
    }
}
