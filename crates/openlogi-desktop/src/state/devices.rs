//! Device-list construction and selection helpers for [`super::AppState`].

use std::collections::HashSet;

use openlogi_camera::Camera;
use openlogi_core::config::{Config, DeviceIdentity};
use openlogi_core::device::{
    BatteryInfo, Capabilities, DeviceInventory, DeviceKind, DeviceModelInfo, DeviceTransports,
    LightCapabilities, StandaloneDevice,
};
use openlogi_core::device_order::{DeviceStableId, PhysicalDeviceKey};
use openlogi_core::hid::DeviceRoute;
use tracing::debug;

use super::device_key::DeviceKey;
use super::inventory::is_fallback_display_name;
use crate::services::assets::{AssetResolver, ResolvedAsset};

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
    /// Route-derived key used for runtime state and, when [`Self::persistent`]
    /// is true, persisted settings.
    pub config_key: String,
    /// Whether `config_key` identifies one physical device and may be written
    /// to configuration. False for a direct/routeless all-zero unit identity.
    pub(crate) persistent: bool,
    /// Stable model key used only for asset/model lookup and diagnostics.
    pub model_key: String,
    pub display_name: String,
    pub asset: Option<ResolvedAsset>,
    pub model_info: Option<DeviceModelInfo>,
    pub codename: Option<String>,
    pub serial_number: Option<String>,
    pub unit_id: [u8; 4],
    /// Standalone driver family, if this is a non-HID++ record.
    pub driver_id: Option<String>,
    /// Model-level asset registry identity for standalone devices.
    pub registry_model_id: Option<String>,
    pub route: Option<DeviceRoute>,
    /// OS capture id for cameras (AVFoundation uniqueID / DirectShow path).
    /// Distinct from [`Self::config_key`], which prefers the port-stable USB
    /// serial. `None` for HID++ devices (those open via [`Self::route`]).
    pub capture_id: Option<String>,
    pub kind: DeviceKind,
    /// Configuration capabilities from the device's HID++ feature table.
    /// Continuity across sleep lives in the hid layer: its probe cache keeps
    /// serving the last-known capabilities for a known-but-offline device, so
    /// this is `None` only for a device never probed since the agent started —
    /// and the UI then falls back to [`Capabilities::presumed_from_kind`].
    pub capabilities: Option<Capabilities>,
    /// Capabilities for standalone non-HID++ controls such as Litra lights.
    pub light_capabilities: Option<LightCapabilities>,
    pub slot: u8,
    pub online: bool,
    pub battery: Option<BatteryInfo>,
}

impl DeviceRecord {
    /// Typed key for `AppState`'s per-device UI caches (DPI/SmartShift load
    /// state, standalone-light overrides, inventory-miss counters). Wraps
    /// [`Self::config_key`] — see [`DeviceKey`].
    pub(crate) fn device_key(&self) -> DeviceKey {
        DeviceKey::from(self.config_key.as_str())
    }

    /// Return the configuration key only when it identifies one physical
    /// device and is therefore safe to persist.
    pub(super) fn persistent_config_key(&self) -> Option<&str> {
        self.persistent.then_some(self.config_key.as_str())
    }

    /// Whether this record may participate in persistent configuration.
    pub(super) fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Key used to reconcile this record across inventory snapshots.
    ///
    /// HID++ devices use [`Self::config_key`]. Cameras may share a model-scoped
    /// config key (two serial-less units of the same model), so they reconcile
    /// on the OS capture id instead — settings still persist under `config_key`.
    pub(super) fn inventory_key(&self) -> String {
        if self.kind == DeviceKind::Camera
            && let Some(id) = self.capture_id.as_deref().filter(|s| !s.is_empty())
        {
            return format!("cam-live:{id}");
        }
        self.config_key.clone()
    }
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
/// *gating* whether it appears at all. See issue #159. Placeholders that are
/// unreachable (their receiver is unplugged), structurally transient, or a
/// legacy same-model duplicate are suppressed — see [`append_offline_known`]
/// (#271/#280/#387).
pub(super) fn build_device_list(
    inventories: &[DeviceInventory],
    standalone: &[StandaloneDevice],
    cache: &AssetResolver,
    config: &Config,
    cameras: &[Camera],
) -> Vec<DeviceRecord> {
    let mut list = Vec::new();
    for inv in inventories {
        for paired in &inv.paired {
            let route = DeviceRoute::device_route_for(inv, paired.slot);
            let (model_key, asset, model_info, codename, serial_number, unit_id) =
                if let Some(model) = paired.model_info.as_ref() {
                    let asset = cache
                        .resolve(model, paired.codename.as_deref())
                        .or_else(|| {
                            // Normal resolution failed (e.g. model_ids are all
                            // zero on HID++ 1.0 devices). Fall back to WPID-based
                            // suffix matching against the asset registry.
                            cache.resolve_by_wpid(
                                paired.wpid,
                                paired.codename.as_deref(),
                            )
                        });
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
                    //
                    // Try resolving assets via wpid suffix or codename: many
                    // Unifying keyboards (K540/K545, K375s, …) are HID++ 1.0 and
                    // lack feature 0x0003, but their wpid still matches the asset
                    // registry's modelId.
                    let key = paired.wpid.map_or_else(
                        || format!("slot{}", paired.slot),
                        |w| format!("wpid{w:04x}"),
                    );
                    let asset =
                        cache.resolve_by_wpid(paired.wpid, paired.codename.as_deref());
                    (key, asset, None, paired.codename.clone(), None, [0u8; 4])
                };
            let stable_id = DeviceStableId::from_parts(
                route.as_ref(),
                paired.slot,
                serial_number.as_deref(),
                unit_id,
            );
            let (config_key, persistent) = stable_id.physical_key().map_or_else(
                || (stable_id.runtime_key(), false),
                |key| (key.into_string(), true),
            );

            let display_name = asset
                .as_ref()
                .map(|a| a.display_name.clone())
                .or_else(|| paired.codename.as_deref().map(prettify_codename))
                .or_else(|| {
                    // Transient resolver failure protection: when the asset
                    // resolver cannot find a match this cycle (e.g. the index
                    // is being rewritten by the sync task), fall back to the
                    // persisted identity's display name if it carries a known
                    // product name. This prevents a transient `None` from
                    // degrading "K540/K545" into "Slot 3".
                    config
                        .device_identity(&config_key)
                        .map(|id| &id.display_name)
                        .filter(|name| !is_fallback_display_name(name))
                        .cloned()
                })
                .unwrap_or_else(|| format!("Slot {}", paired.slot));
            let kind = effective_kind(paired.kind, asset.as_ref().map(|a| a.kind));
            list.push(DeviceRecord {
                config_key,
                persistent,
                model_key,
                display_name,
                asset,
                model_info,
                codename,
                serial_number,
                unit_id,
                driver_id: None,
                registry_model_id: None,
                route,
                capture_id: None,
                kind,
                capabilities: paired.capabilities,
                light_capabilities: None,
                slot: paired.slot,
                online: paired.online,
                battery: paired.battery.clone(),
            });
        }
    }
    append_standalone(&mut list, standalone, cache);
    #[cfg(debug_assertions)]
    if std::env::var_os("OPENLOGI_DEMO_KEYBOARD").is_some() {
        list.push(demo_keyboard());
    }
    let present_receivers: HashSet<String> = inventories
        .iter()
        .filter_map(|inv| inv.receiver.unique_id.as_deref())
        .map(str::to_ascii_lowercase)
        .collect();
    append_offline_known(
        &mut list,
        config.known_identities(),
        cache,
        &present_receivers,
    );
    // Cameras are UVC, not HID++, so they come from a parallel discovery path
    // (AVFoundation on macOS) rather than the receiver inventory. The caller
    // enumerates them off the UI thread — discovery is too slow for the render
    // path — so this assembly stays pure; the merge in
    // `super::AppState::refresh_inventories` reconciles them by config_key.
    for camera in cameras {
        list.push(camera_record(camera, cache));
    }
    sort_device_list(&mut list);
    list
}

/// A [`DeviceRecord`] for a Logitech UVC webcam.
///
/// [`Camera::config_key`] prefers the USB serial so saved controls survive a
/// port change; [`DeviceRecord::capture_id`] keeps the OS open id the preview
/// and UVC layer need. `route: None` / `capabilities: None` keep it out of
/// every HID++ path — its only detail surface is the live preview tab.
///
/// The asset registry keys cameras by their 4-hex USB product id (e.g. the
/// StreamCam's `0893`), so a webcam's product render resolves through the same
/// [`AssetResolver`] as HID++ devices once we synthesize a minimal
/// [`DeviceModelInfo`] from the USB pid.
fn camera_record(camera: &Camera, cache: &AssetResolver) -> DeviceRecord {
    let config_key = camera.config_key();
    let model_info = camera_model_info(camera);
    let asset = cache.resolve(&model_info, Some(&camera.name));
    DeviceRecord {
        model_key: format!("{:04x}", camera.product_id),
        config_key,
        persistent: true,
        display_name: camera.name.clone(),
        asset,
        model_info: None,
        codename: None,
        serial_number: camera.serial_number.clone(),
        unit_id: [0; 4],
        driver_id: None,
        registry_model_id: None,
        route: None,
        capture_id: Some(camera.unique_id.clone()),
        kind: DeviceKind::Camera,
        capabilities: None,
        light_capabilities: None,
        slot: 0,
        online: true,
        battery: None,
    }
}

/// A minimal [`DeviceModelInfo`] standing in for a UVC camera, carrying just the
/// USB product id in `model_ids[0]` so [`AssetResolver::resolve`] can match the
/// registry's camera depots (which key on the 4-hex pid).
pub(crate) fn camera_model_info(camera: &Camera) -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports::default(),
        model_ids: [camera.product_id, 0, 0],
        extended_model_id: 0,
    }
}

fn append_standalone(
    list: &mut Vec<DeviceRecord>,
    devices: &[StandaloneDevice],
    cache: &AssetResolver,
) {
    for device in devices {
        let route = Some(DeviceRoute::RawHid {
            vendor_id: device.address.vendor_id,
            product_id: device.address.product_id,
            usage_page: device.address.usage_page,
            usage_id: device.address.usage_id,
            identity: device.address.identity.clone(),
        });
        let stable_id = DeviceStableId::from_parts(
            route.as_ref(),
            openlogi_core::hid::DIRECT_DEVICE_INDEX,
            device.serial_number.as_deref(),
            device.unit_id,
        );
        let (config_key, persistent) = stable_id.physical_key().map_or_else(
            || (stable_id.runtime_key(), false),
            |key| (key.into_string(), true),
        );
        let asset = device
            .registry_model_id
            .as_deref()
            .and_then(|model_id| cache.resolve_registry_model(model_id));
        let display_name = asset
            .as_ref()
            .filter(|asset| !asset.display_name.trim().is_empty())
            .map_or_else(
                || device.display_name.clone(),
                |asset| asset.display_name.clone(),
            );
        list.push(DeviceRecord {
            config_key,
            persistent,
            // The registry id is presentation metadata, not a replacement for
            // the raw-device model identity used before registry integration.
            model_key: format!("raw:{:04x}", device.address.product_id),
            display_name,
            asset,
            model_info: None,
            codename: None,
            serial_number: device.serial_number.clone(),
            unit_id: device.unit_id,
            driver_id: Some(device.driver_id.clone()),
            registry_model_id: device.registry_model_id.clone(),
            route,
            capture_id: None,
            kind: device.kind,
            capabilities: device.capabilities,
            light_capabilities: device.light_capabilities,
            slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
            online: device.online,
            battery: None,
        });
    }
}

/// Append an offline placeholder for every known device not already present in
/// `list`, skipping unreachable devices and invalid transient identities.
///
/// The gates keep phantom cards out without conflating model identity with
/// physical identity:
/// - an exact physical key match against a live record — the device is already
///   in the list;
/// - a `receiver:` key whose receiver is not plugged in — its paired devices
///   are unreachable until that receiver returns (e.g. the work receiver's
///   mouse while at home);
/// - a historical direct/routeless all-zero unit key, which never identified a
///   physical device;
/// - for legacy model-scoped keys only, a model/PID already visible live or as
///   an earlier placeholder. This preserves the #271/#280 compatibility fix
///   without hiding a second physical device of the same model.
fn append_offline_known<'a>(
    list: &mut Vec<DeviceRecord>,
    known: impl Iterator<Item = (&'a str, &'a DeviceIdentity)>,
    cache: &AssetResolver,
    present_receivers: &HashSet<String>,
) {
    let mut present_keys: HashSet<String> = list
        .iter()
        .map(|record| record.config_key.clone())
        .collect();
    let mut blocked_legacy_models: HashSet<String> =
        list.iter().map(|record| record.model_key.clone()).collect();
    let mut blocked_legacy_pids: HashSet<String> =
        list.iter().filter_map(record_wire_pid).collect();
    let mut known = known.collect::<Vec<_>>();
    known.sort_by_key(|(key, identity)| (identity.model_info.is_none(), (*key).to_string()));

    for (key, identity) in known {
        if PhysicalDeviceKey::is_transient(key) {
            continue;
        }
        if receiver_uid_of(key).is_some_and(|uid| !present_receivers.contains(&uid)) {
            continue;
        }
        if present_keys.contains(key) {
            continue;
        }
        let is_legacy_model_key = PhysicalDeviceKey::parse(key).is_none();
        let model_key = identity
            .model_info
            .as_ref()
            .map_or_else(|| key.to_string(), DeviceModelInfo::config_key);
        if is_legacy_model_key && blocked_legacy_models.contains(&model_key) {
            continue;
        }
        let record = offline_record(key, identity, cache);
        let wire_pid = record_wire_pid(&record);
        if is_legacy_model_key
            && wire_pid
                .as_ref()
                .is_some_and(|pid| blocked_legacy_pids.contains(pid))
        {
            continue;
        }
        present_keys.insert(record.config_key.clone());
        blocked_legacy_models.insert(record.model_key.clone());
        if let Some(pid) = wire_pid {
            blocked_legacy_pids.insert(pid);
        }
        list.push(record);
    }
}

/// The receiver UID embedded in a `receiver:<uid>:slot:<n>` config key.
fn receiver_uid_of(key: &str) -> Option<String> {
    key.strip_prefix("receiver:")
        .and_then(|rest| rest.split(':').next())
        .map(str::to_ascii_lowercase)
}

/// The record's wire product id, used to suppress legacy same-model duplicate
/// cards without conflating physical device keys.
fn record_wire_pid(record: &DeviceRecord) -> Option<String> {
    match record.model_info.as_ref().map(|m| m.model_ids[0]) {
        Some(pid) if pid != 0 => Some(format!("{pid:04x}")),
        // A degenerate `model_ids[0] == 0` falls through to `None` (no PID dedup);
        // the record still dedups by key, so two identical zero-id models showing
        // as separate offline cards is a rare, accepted gap.
        _ => record
            .model_key
            .strip_prefix("wpid")
            .map(str::to_ascii_lowercase),
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
    let asset = identity
        .registry_model_id
        .as_deref()
        .and_then(|model_id| cache.resolve_registry_model(model_id))
        .or_else(|| {
            model_info
                .as_ref()
                .and_then(|model| cache.resolve(model, identity.codename.as_deref()))
        });
    // Keep offline standalone records keyed exactly as before. The registry id
    // only selects artwork and must not alter configuration or deduplication.
    let model_key = model_info
        .as_ref()
        .map_or_else(|| config_key.to_string(), DeviceModelInfo::config_key);
    let display_name = asset
        .as_ref()
        .filter(|asset| !asset.display_name.trim().is_empty())
        .map_or_else(
            || identity.display_name.clone(),
            |asset| asset.display_name.clone(),
        );
    DeviceRecord {
        config_key: config_key.to_string(),
        persistent: true,
        model_key,
        display_name,
        asset,
        model_info,
        codename: identity.codename.clone(),
        serial_number: None,
        unit_id: [0; 4],
        driver_id: identity.driver_id.clone(),
        registry_model_id: identity.registry_model_id.clone(),
        route: None,
        capture_id: None,
        kind: identity.kind,
        capabilities: Some(identity.capabilities),
        light_capabilities: identity.light_capabilities,
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

/// The `direct:<vid>:<pid>` prefix of a direct config key, or `None` for any
/// other key shape. Two keys sharing a prefix name the same wire product.
pub(super) fn direct_key_prefix(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("direct:")?;
    let (vid, rest) = rest.split_once(':')?;
    let (pid, identity) = rest.split_once(':')?;
    (!vid.is_empty() && !pid.is_empty() && !identity.is_empty())
        .then(|| &key[..key.len() - identity.len() - 1])
}

/// Fold a transient live record into the known card it physically is: the card
/// keeps its persisted identity while the live record supplies volatile state.
pub(super) fn adopt_transient_record(known: &DeviceRecord, live: DeviceRecord) -> DeviceRecord {
    DeviceRecord {
        config_key: known.config_key.clone(),
        persistent: true,
        model_key: known.model_key.clone(),
        display_name: known.display_name.clone(),
        asset: known.asset.clone().or(live.asset),
        model_info: known.model_info.clone().or(live.model_info),
        codename: known.codename.clone().or(live.codename),
        serial_number: known.serial_number.clone().or(live.serial_number),
        unit_id: known.unit_id,
        driver_id: live.driver_id.or_else(|| known.driver_id.clone()),
        registry_model_id: live
            .registry_model_id
            .or_else(|| known.registry_model_id.clone()),
        route: live.route,
        capture_id: live.capture_id.or_else(|| known.capture_id.clone()),
        kind: if known.kind == DeviceKind::Unknown {
            live.kind
        } else {
            known.kind
        },
        capabilities: live.capabilities.or(known.capabilities),
        light_capabilities: live.light_capabilities.or(known.light_capabilities),
        slot: live.slot,
        online: live.online,
        battery: live.battery.or_else(|| known.battery.clone()),
    }
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
        persistent: true,
        model_key: "demo-g513".to_string(),
        display_name: "Logitech G513".to_string(),
        asset: None,
        model_info: None,
        codename: None,
        serial_number: None,
        unit_id: [0; 4],
        driver_id: None,
        registry_model_id: None,
        route: None,
        capture_id: None,
        kind: DeviceKind::Keyboard,
        capabilities: Some(Capabilities {
            lighting: true,
            ..Capabilities::default()
        }),
        light_capabilities: None,
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
/// `resolve_device_kind` (`crates/openlogi-hid/src/inventory/mappings.rs`); this applies
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
        .and_then(|key| {
            list.iter()
                .position(|record| record.is_persistent() && record.config_key == key)
        })
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
    use openlogi_core::device::{
        DeviceInventory, PairedDevice, RawDeviceAddress, ReceiverInfo, StandaloneDevice,
    };

    use crate::services::assets::AssetResolver;

    use std::collections::HashSet;

    use super::{
        Camera, Capabilities, DeviceIdentity, DeviceKind, DeviceModelInfo, DeviceRecord,
        DeviceTransports, append_offline_known, build_device_list, direct_key_prefix,
        effective_kind, offline_record, pick_initial_device,
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

    fn direct_inventory(model_info: DeviceModelInfo) -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "MX Master 3S".into(),
                vendor_id: 0x046d,
                product_id: 0xb023,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: openlogi_core::hid::DIRECT_DEVICE_INDEX,
                codename: Some("MX Master 3S".into()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: None,
                model_info: Some(model_info),
                capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
            }],
        }
    }

    fn online_record(key: &str) -> DeviceRecord {
        DeviceRecord {
            config_key: key.to_string(),
            persistent: true,
            model_key: key.to_string(),
            display_name: format!("live {key}"),
            asset: None,
            model_info: None,
            codename: None,
            serial_number: None,
            unit_id: [1; 4],
            driver_id: None,
            registry_model_id: None,
            route: None,
            capture_id: None,
            kind: DeviceKind::Mouse,
            capabilities: Some(Capabilities::presumed_from_kind(DeviceKind::Mouse)),
            light_capabilities: None,
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
                hires_wheel: false,
                thumbwheel: false,
                haptic_feedback: false,
                haptic_panel: false,
            },
            light_capabilities: None,
            model_info: None,
            codename: None,
            driver_id: None,
            registry_model_id: None,
        }
    }

    #[test]
    fn standalone_registry_identity_is_preserved_without_hidpp_model_info() {
        let device = StandaloneDevice {
            address: RawDeviceAddress {
                vendor_id: 0x046d,
                product_id: 0xc901,
                usage_page: 0xff43,
                usage_id: 0x0202,
                identity: "serial:beam-1".into(),
            },
            display_name: "Future Litra model".into(),
            manufacturer: Some("Logi".into()),
            serial_number: Some("beam-1".into()),
            unit_id: [0; 4],
            kind: DeviceKind::Light,
            online: true,
            capabilities: None,
            light_capabilities: None,
            driver_id: "litra".into(),
            registry_model_id: Some("8c901".into()),
        };
        let list = build_device_list(
            &[],
            std::slice::from_ref(&device),
            &AssetResolver::new(),
            &Config::default(),
            &[],
        );

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].driver_id.as_deref(), Some("litra"));
        assert_eq!(list[0].registry_model_id.as_deref(), Some("8c901"));
        assert_eq!(list[0].model_key, "raw:c901");
        assert_eq!(list[0].config_key, "raw:046d:c901:ff43:0202:serial:beam-1");
        assert!(list[0].asset.is_none());
    }

    #[test]
    fn no_model_info_uses_receiver_slot_as_config_key() {
        let inv = inventory_with(vec![paired_device_no_model_info(1, Some(0x4076))]);
        let cache = AssetResolver::new();
        let list = build_device_list(&[inv], &[], &cache, &Config::default(), &[]);
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
        let list = build_device_list(&[inv], &[], &cache, &Config::default(), &[]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].config_key, "receiver:da2699e1:slot:3");
        assert_eq!(list[0].model_key, "slot3");
    }

    #[test]
    fn no_model_info_display_name_falls_back_to_slot() {
        let inv = inventory_with(vec![paired_device_no_model_info(2, Some(0x4051))]);
        let cache = AssetResolver::new();
        let list = build_device_list(&[inv], &[], &cache, &Config::default(), &[]);
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
    fn offline_standalone_record_keeps_registry_and_physical_keys() {
        let id = DeviceIdentity {
            display_name: "Litra Glow".into(),
            kind: DeviceKind::Light,
            capabilities: Capabilities::default(),
            light_capabilities: None,
            model_info: None,
            codename: None,
            driver_id: Some("litra".into()),
            registry_model_id: Some("8c900".into()),
        };
        let record = offline_record(
            "raw:046d:c900:ff43:0202:serial:known-light",
            &id,
            &AssetResolver::new(),
        );

        assert_eq!(record.registry_model_id.as_deref(), Some("8c900"));
        assert_eq!(
            record.model_key,
            "raw:046d:c900:ff43:0202:serial:known-light"
        );
        assert_eq!(
            record.config_key,
            "raw:046d:c900:ff43:0202:serial:known-light"
        );
        assert!(record.model_info.is_none());
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
        append_offline_known(
            &mut list,
            [("A", &a), ("B", &b)].into_iter(),
            &cache,
            &HashSet::new(),
        );

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

    fn model_info(ext: u8, pid: u16) -> DeviceModelInfo {
        DeviceModelInfo {
            entity_count: 0,
            serial_number: None,
            unit_id: [0; 4],
            transports: DeviceTransports::default(),
            model_ids: [pid, 0, 0],
            extended_model_id: ext,
        }
    }

    #[test]
    fn zero_unit_direct_inventory_is_transient() {
        let cache = AssetResolver::new();
        let list = build_device_list(
            &[direct_inventory(model_info(2, 0xb034))],
            &[],
            &cache,
            &Config::default(),
            &[],
        );

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].config_key, "direct:046d:b023:unit:00000000");
        assert!(!list[0].is_persistent());
        assert!(list[0].persistent_config_key().is_none());
    }

    #[test]
    fn historical_zero_unit_identity_does_not_create_offline_card() {
        let id = mouse_identity("MX Master 3S");
        let cache = AssetResolver::new();
        let mut list = Vec::new();

        append_offline_known(
            &mut list,
            [("direct:046d:b023:unit:00000000", &id)].into_iter(),
            &cache,
            &HashSet::new(),
        );

        assert!(list.is_empty());
    }

    #[test]
    fn same_model_physical_bluetooth_devices_remain_distinct() {
        let mut id_a = mouse_identity("MX Master 3S");
        id_a.model_info = Some(model_info(2, 0xb034));
        let id_b = id_a.clone();
        let cache = AssetResolver::new();
        let mut list = Vec::new();

        append_offline_known(
            &mut list,
            [
                ("direct:046d:b023:unit:01020304", &id_a),
                ("direct:046d:b023:unit:05060708", &id_b),
            ]
            .into_iter(),
            &cache,
            &HashSet::new(),
        );

        assert_eq!(list.len(), 2);
    }

    #[test]
    fn persisted_selection_does_not_target_transient_identity() {
        let stable = online_record("receiver:aabb:slot:1");
        let mut transient = online_record("direct:046d:b023:unit:00000000");
        transient.persistent = false;
        let list = vec![stable, transient];

        assert_eq!(
            pick_initial_device(&list, Some("direct:046d:b023:unit:00000000")),
            0
        );
    }

    #[test]
    fn placeholders_for_absent_receivers_are_hidden() {
        // The work receiver's mouse must not haunt the list at home: with its
        // receiver unplugged the device is unreachable, so no card is shown.
        let id = mouse_identity("MX Master 3S");
        let cache = AssetResolver::new();
        let mut list = Vec::new();
        append_offline_known(
            &mut list,
            [("receiver:aabb:slot:1", &id)].into_iter(),
            &cache,
            &HashSet::new(),
        );
        assert!(list.is_empty());
        append_offline_known(
            &mut list,
            [("receiver:aabb:slot:1", &id)].into_iter(),
            &cache,
            &HashSet::from(["aabb".to_string()]),
        );
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn same_model_placeholder_is_blocked_by_a_live_unit() {
        // #271: the live mouse reads ext-model 02 while the stale identity was
        // recorded as 00 — the wire PID still identifies them as one model, so
        // the phantom card is suppressed.
        let mut live = online_record("receiver:aabb:slot:2");
        live.model_key = "2b034".to_string();
        live.model_info = Some(model_info(2, 0xb034));
        let mut list = vec![live];
        let id = mouse_identity("MX Master 3S");
        let cache = AssetResolver::new();
        append_offline_known(
            &mut list,
            [("0b034", &id)].into_iter(),
            &cache,
            &HashSet::new(),
        );
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn legacy_same_model_placeholders_collapse_to_one_card() {
        // Two persisted identities of one model render identically — a second
        // offline card carries no information, only confusion.
        let id_a = mouse_identity("MX Master 3S");
        let id_b = mouse_identity("MX Master 3S");
        let cache = AssetResolver::new();
        let mut list = Vec::new();
        append_offline_known(
            &mut list,
            [("0b034", &id_a), ("2b034", &id_b)].into_iter(),
            &cache,
            &HashSet::new(),
        );
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn direct_key_prefix_names_the_wire_product() {
        assert_eq!(
            direct_key_prefix("direct:046d:c09d:unit:46002e00"),
            Some("direct:046d:c09d")
        );
        assert_eq!(
            direct_key_prefix("direct:046d:c09d:serial:abc123"),
            Some("direct:046d:c09d")
        );
        assert_eq!(
            direct_key_prefix("direct:046d:c09d:unit:00000000"),
            Some("direct:046d:c09d"),
            "transient keys share the prefix of their physical siblings"
        );
    }

    #[test]
    fn non_direct_keys_have_no_wire_prefix() {
        assert_eq!(direct_key_prefix("receiver:da2699e1:slot:1"), None);
        assert_eq!(direct_key_prefix("unknown:slot:0:unit:00000000"), None);
        assert_eq!(direct_key_prefix("2b034"), None);
        assert_eq!(direct_key_prefix("direct:046d:c09d:"), None);
        assert_eq!(direct_key_prefix("direct:046d"), None);
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

    #[test]
    fn webcams_are_appended_as_camera_records() {
        // A discovered UVC webcam joins the list as a routeless Camera record.
        // With a USB serial the config key is port-stable; capture_id keeps the
        // OS open id the preview needs.
        let camera = Camera {
            name: "Logitech StreamCam".to_string(),
            unique_id: "0x1123000046d0893".to_string(),
            serial_number: Some("ABC123".to_string()),
            vendor_id: 0x046d,
            product_id: 0x0893,
            max_resolution: Some((1920, 1080)),
            max_fps: Some(60),
        };
        let cache = AssetResolver::new();
        let list = build_device_list(&[], &[], &cache, &Config::default(), &[camera]);

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, DeviceKind::Camera);
        assert_eq!(list[0].config_key, "camera:046d:0893:serial:abc123");
        assert_eq!(list[0].capture_id.as_deref(), Some("0x1123000046d0893"));
        assert_eq!(list[0].serial_number.as_deref(), Some("ABC123"));
        assert_eq!(list[0].display_name, "Logitech StreamCam");
        assert!(list[0].route.is_none());
        assert!(list[0].capabilities.is_none());
        assert!(list[0].online);
    }

    #[test]
    fn webcam_without_serial_uses_model_scoped_key() {
        let camera = Camera {
            name: "Logitech C920".to_string(),
            unique_id: "0x14110000046d082d".to_string(),
            serial_number: None,
            vendor_id: 0x046d,
            product_id: 0x082d,
            max_resolution: None,
            max_fps: None,
        };
        let cache = AssetResolver::new();
        let list = build_device_list(&[], &[], &cache, &Config::default(), &[camera]);
        // Port-stable even without a serial: settings follow the model, not the
        // OS capture id (which embeds the USB location on macOS/Windows).
        assert_eq!(list[0].config_key, "camera:046d:082d");
        assert_eq!(list[0].capture_id.as_deref(), Some("0x14110000046d082d"));
    }

    #[test]
    fn webcam_config_key_survives_a_usb_port_change() {
        let port_a = Camera {
            name: "Logitech StreamCam".to_string(),
            unique_id: "0x1123000046d0893".to_string(),
            serial_number: Some("SN42".to_string()),
            vendor_id: 0x046d,
            product_id: 0x0893,
            max_resolution: None,
            max_fps: None,
        };
        let port_b = Camera {
            unique_id: "0x14110000046d0893".to_string(),
            ..port_a.clone()
        };
        let cache = AssetResolver::new();
        let a = build_device_list(&[], &[], &cache, &Config::default(), &[port_a]);
        let b = build_device_list(&[], &[], &cache, &Config::default(), &[port_b]);
        assert_eq!(a[0].config_key, b[0].config_key);
        assert_ne!(a[0].capture_id, b[0].capture_id);
    }

    #[test]
    fn two_serial_less_same_model_cameras_stay_distinct() {
        // Settings share the model key (no USB serial to go on); inventory
        // identity uses capture_id so both still appear in the list.
        let a = Camera {
            name: "Logitech StreamCam".to_string(),
            unique_id: "0x1123000046d0893".to_string(),
            serial_number: None,
            vendor_id: 0x046d,
            product_id: 0x0893,
            max_resolution: None,
            max_fps: None,
        };
        let b = Camera {
            unique_id: "0x14110000046d0893".to_string(),
            ..a.clone()
        };
        let cache = AssetResolver::new();
        let list = build_device_list(&[], &[], &cache, &Config::default(), &[a, b]);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].config_key, list[1].config_key);
        assert_eq!(list[0].config_key, "camera:046d:0893");
        assert_ne!(list[0].inventory_key(), list[1].inventory_key());
        assert_ne!(list[0].capture_id, list[1].capture_id);
    }
}
