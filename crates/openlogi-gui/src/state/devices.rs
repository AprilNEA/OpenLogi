//! Device-list construction and selection helpers for [`super::AppState`].

use openlogi_agent_core::device_order::DeviceStableId;
use openlogi_core::device::{BatteryInfo, Capabilities, DeviceInventory, DeviceKind};
use openlogi_hid::{DIRECT_DEVICE_INDEX, DeviceRoute};
use tracing::debug;

use crate::asset::{AssetResolver, ResolvedAsset};

/// One paired device with everything the UI needs to switch to it in O(1):
/// the config key (for bindings/DPI persistence), a display name, the
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
    pub config_key: String,
    pub display_name: String,
    pub asset: Option<ResolvedAsset>,
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

pub(super) fn build_device_list(
    inventories: &[DeviceInventory],
    cache: &AssetResolver,
) -> Vec<DeviceRecord> {
    let mut list = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            let config_key = model.config_key();
            let asset = cache.resolve(model, paired.codename.as_deref());
            let display_name = asset
                .as_ref()
                .map(|a| a.display_name.clone())
                .or_else(|| paired.codename.as_deref().map(prettify_codename))
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            let kind = effective_kind(paired.kind, asset.as_ref().map(|a| a.kind));
            list.push(DeviceRecord {
                config_key,
                display_name,
                asset,
                serial_number: model.serial_number.clone(),
                unit_id: model.unit_id,
                route: device_route(inv, paired.slot),
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
    sort_device_list(&mut list);
    list
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
        record.config_key.clone(),
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
        display_name: "Logitech G513".to_string(),
        asset: None,
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

/// Build the [`DeviceRoute`] HID++ writes use to reach a device.
///
/// A Bolt-paired device routes through its receiver UID + slot. A directly
/// attached one (USB cable / Bluetooth) carries no receiver UID and sits at
/// [`DIRECT_DEVICE_INDEX`] — it routes by the HID node's vendor/product id
/// instead. A Bolt device whose receiver UID couldn't be read gets no route
/// (`None`), so hardware writes are skipped rather than mis-routed to the
/// receiver's own pid.
fn device_route(inv: &DeviceInventory, slot: u8) -> Option<DeviceRoute> {
    match &inv.receiver.unique_id {
        Some(receiver_uid) => Some(DeviceRoute::Bolt {
            receiver_uid: receiver_uid.clone(),
            slot,
        }),
        None if slot == DIRECT_DEVICE_INDEX => Some(DeviceRoute::Direct {
            vendor_id: inv.receiver.vendor_id,
            product_id: inv.receiver.product_id,
        }),
        None => None,
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
    use super::{DeviceKind, effective_kind};

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
