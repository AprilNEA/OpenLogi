use std::{fmt::Write as _, future::Future, sync::Arc, time::Duration};

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature, device_information::DeviceInformationFeature,
        device_type_and_name::DeviceTypeAndNameFeature, unified_battery::UnifiedBatteryFeature,
    },
    feature::{hires_wheel::HiResWheelFeature, root::FeatureInformation},
};
use openlogi_core::device::{
    BatteryInfo, Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports,
};
use tokio::time::timeout;
use tracing::debug;

use crate::mappings::{
    map_battery_level, map_battery_status, map_device_type, normalize_serial_number,
};
use crate::route::DIRECT_DEVICE_INDEX;

/// Optional per-feature reads must not consume the whole node probe budget.
///
/// BLE-direct devices can occasionally drop or delay one HID++ response while
/// still being otherwise usable. The feature table is the gate for "this is a
/// configurable device"; battery, wheel-inversion, serial and marketing type are
/// useful adornments and should degrade independently.
const OPTIONAL_FEATURE_TIMEOUT: Duration = Duration::from_millis(1500);
const DIRECT_CAPABILITY_TIMEOUT: Duration = Duration::from_millis(300);

const DIRECT_CAPABILITY_FEATURE_IDS: &[u16] = &[
    0x1b00, 0x1b01, 0x1b02, 0x1b03, 0x1b04, // reprogrammable controls
    0x2201, 0x2202, // adjustable DPI / pointer resolution
    0x8070, 0x8080, // keyboard lighting families OpenLogi can drive
];
const DIRECT_POINTER_CAPABILITY_FEATURE_IDS: &[u16] = &[
    0x1b00, 0x1b01, 0x1b02, 0x1b03, 0x1b04, // reprogrammable controls
    0x2201, 0x2202, // adjustable DPI / pointer resolution
];
const DIRECT_KEYBOARD_CAPABILITY_FEATURE_IDS: &[u16] = &[
    0x8070, 0x8080, // keyboard lighting families OpenLogi can drive
];

/// Everything a single device probe yields. Any field is `None` when the
/// device doesn't expose that feature or the read failed.
#[derive(Default, Clone)]
pub(super) struct ProbedFeatures {
    pub(super) battery: Option<BatteryInfo>,
    pub(super) model_info: Option<DeviceModelInfo>,
    /// Marketing type from HID++ `0x0005` — an identity hint only.
    pub(super) kind: Option<DeviceKind>,
    /// Configuration capabilities derived from the device's feature table.
    pub(super) capabilities: Option<Capabilities>,
    /// Direct-device discriminator evidence from measured facts only. Kind-based
    /// fallback capabilities keep an accepted mouse configurable, but they are
    /// not enough to prove that a macOS HID interface is the primary peripheral.
    pub(super) direct_peripheral_evidence: bool,
}

fn format_feature_ids(ids: &[u16]) -> String {
    let mut out = String::with_capacity(ids.len().saturating_mul(7).saturating_sub(2));
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(&mut out, "{id:#06x}");
    }
    out
}

/// Read just the battery by addressing the `UnifiedBattery` feature at its
/// known runtime `feature_index` — one round-trip, with no `Device::new` ping
/// and no feature-table walk. This is both the full probe's battery read (the
/// walk just produced the index) and the cheap per-tick refresh for cache hits.
/// `None` when the device doesn't answer (asleep, switched hosts).
pub(super) async fn read_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    feature_index: u8,
) -> Option<BatteryInfo> {
    let feature = UnifiedBatteryFeature::new(Arc::clone(channel), slot, feature_index);
    optional_read(
        slot,
        "UnifiedBattery getBatteryInfo",
        feature.get_battery_info(),
    )
    .await
    .map(|info| BatteryInfo {
        percentage: info.charging_percentage,
        level: map_battery_level(info.level),
        status: map_battery_status(info.status),
    })
}

/// Runtime index of the `UnifiedBattery` feature in an enumerated feature-ID
/// table, for [`read_battery`]. The table is 1-based (index 0 is the implicit
/// root feature, which enumeration omits).
pub(super) fn battery_feature_index(ids: impl IntoIterator<Item = u16>) -> Option<u8> {
    ids.into_iter()
        .position(|id| id == UnifiedBatteryFeature::ID)
        // A feature table holds at most `u8::MAX` entries (its count is a u8),
        // so the 1-based index always fits.
        .and_then(|pos| u8::try_from(pos + 1).ok())
}

/// Open a HID++ session for `slot` and read everything we care about (battery,
/// device-information, `0x0005` device type, and the feature table that drives
/// [`Capabilities`]) in one shot. Device sessions are expensive (multi-round-
/// trip) so we fold every read through the same `Device::new` +
/// `enumerate_features` — the feature table is the Vec that enumeration already
/// returns, so capabilities cost no extra round-trip.
///
/// Also returns the `UnifiedBattery` runtime index found by the walk, so later
/// ticks can refresh the battery without repeating it.
///
/// Only online, responsive devices reach here.
pub(super) async fn probe_features(
    channel: &Arc<HidppChannel>,
    slot: u8,
) -> (ProbedFeatures, Option<u8>) {
    let mut device = match Device::new(Arc::clone(channel), slot).await {
        Ok(d) => d,
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            return (ProbedFeatures::default(), None);
        }
    };
    debug!(slot, "Device::new succeeded");
    if slot == DIRECT_DEVICE_INDEX {
        return probe_direct_features(channel, &device, slot).await;
    }
    // The enumeration response IS the device's feature-ID table — capture it
    // for capability derivation instead of discarding it.
    let mut battery_index = None;
    let mut capabilities = match device.enumerate_features().await {
        Ok(Some(features)) => {
            let ids: Vec<u16> = features.iter().map(|f| f.id).collect();
            battery_index = battery_feature_index(ids.iter().copied());
            let caps = Capabilities::from_feature_ids(&ids);
            debug!(
                slot,
                feature_count = ids.len(),
                feature_ids = %format_feature_ids(&ids),
                battery_index,
                capabilities = ?caps,
                "feature table enumerated"
            );
            Some(caps)
        }
        Ok(None) => {
            debug!(slot, "feature enumeration returned no table");
            None
        }
        Err(e) => {
            debug!(slot, error = ?e, "enumerate_features failed");
            return (ProbedFeatures::default(), None);
        }
    };
    // Identity is essential for the GUI/agent to treat this as the live device.
    // Read it before volatile adornments such as battery; a flaky battery query
    // must not leave a BLE-direct mouse looking offline.
    let model_info = read_model_info(&device, slot).await;

    if let Some(caps) = capabilities.as_mut()
        && let Some(feature) = device.get_feature::<HiResWheelFeature>()
    {
        caps.scroll_inversion = optional_read(
            slot,
            "HiResWheel getWheelCapabilities",
            feature.get_wheel_capabilities(),
        )
        .await
        .is_some_and(|wheel| wheel.has_invert);
    }

    let battery = match battery_index {
        Some(feature_index) => read_battery(channel, slot, feature_index).await,
        None => None,
    };

    // `0x0005` reports the device's own marketing type (mouse, keyboard, …) —
    // the authoritative kind signal. On the direct path it's the only one; on
    // the Bolt path it corrects a pairing register that reported the wrong (or
    // `Unknown`) kind.
    let kind = match device.get_feature::<DeviceTypeAndNameFeature>() {
        Some(feature) => optional_read(
            slot,
            "DeviceTypeAndName getDeviceType",
            feature.get_device_type(),
        )
        .await
        .map(map_device_type),
        None => None,
    };

    (
        ProbedFeatures {
            battery,
            model_info,
            kind,
            capabilities,
            direct_peripheral_evidence: false,
        },
        battery_index,
    )
}

async fn read_model_info(device: &Device, slot: u8) -> Option<DeviceModelInfo> {
    let feature = device.get_feature::<DeviceInformationFeature>()?;
    read_model_info_from_feature(&feature, slot).await
}

async fn read_model_info_from_feature(
    feature: &DeviceInformationFeature,
    slot: u8,
) -> Option<DeviceModelInfo> {
    let info = optional_read(
        slot,
        "DeviceInformation getDeviceInfo",
        feature.get_device_info(),
    )
    .await?;
    let serial_number = if info.capabilities.serial_number {
        optional_read(
            slot,
            "DeviceInformation getSerialNumber",
            feature.get_serial_number(),
        )
        .await
        .and_then(|serial| normalize_serial_number(&serial))
    } else {
        None
    };
    let model = DeviceModelInfo {
        entity_count: info.entity_count,
        serial_number,
        unit_id: info.unit_id,
        transports: DeviceTransports {
            usb: info.transport.usb,
            equad: info.transport.e_quad,
            btle: info.transport.btle,
            bluetooth: info.transport.bluetooth,
        },
        model_ids: info.model_id,
        extended_model_id: info.extended_model_id,
    };
    if !model.has_model_identity() {
        debug!(
            slot,
            entity_count = model.entity_count,
            unit_id = format_args!(
                "{:02x}{:02x}{:02x}{:02x}",
                model.unit_id[0],
                model.unit_id[1],
                model.unit_id[2],
                model.unit_id[3]
            ),
            model_ids = ?model.model_ids,
            extended_model_id = model.extended_model_id,
            "DeviceInformation payload has no model id — ignoring as non-identifying"
        );
        return None;
    }
    Some(model)
}

async fn probe_direct_features(
    channel: &Arc<HidppChannel>,
    device: &Device,
    slot: u8,
) -> (ProbedFeatures, Option<u8>) {
    let mut ids = Vec::new();

    let model_info = match direct_feature_info(
        device,
        slot,
        DeviceInformationFeature::ID,
        "Root getFeature DeviceInformation",
    )
    .await
    {
        Some(info) => {
            ids.push(DeviceInformationFeature::ID);
            let feature = DeviceInformationFeature::new(Arc::clone(channel), slot, info.index);
            read_model_info_from_feature(&feature, slot).await
        }
        None => None,
    };

    let mut battery = None;
    let mut battery_index = None;
    if let Some(info) = direct_feature_info(
        device,
        slot,
        UnifiedBatteryFeature::ID,
        "Root getFeature UnifiedBattery",
    )
    .await
    {
        ids.push(UnifiedBatteryFeature::ID);
        battery_index = Some(info.index);
        battery = read_battery(channel, slot, info.index).await;
    }

    let mut kind = None;
    if let Some(info) = direct_feature_info(
        device,
        slot,
        DeviceTypeAndNameFeature::ID,
        "Root getFeature DeviceTypeAndName",
    )
    .await
    {
        ids.push(DeviceTypeAndNameFeature::ID);
        let feature = DeviceTypeAndNameFeature::new(Arc::clone(channel), slot, info.index);
        kind = optional_read(
            slot,
            "DeviceTypeAndName getDeviceType",
            feature.get_device_type(),
        )
        .await
        .map(map_device_type);
    }

    for &id in direct_capability_probe_ids(kind) {
        if direct_feature_info_with_timeout(
            device,
            slot,
            id,
            "Root getFeature capability",
            DIRECT_CAPABILITY_TIMEOUT,
        )
        .await
        .is_some()
        {
            ids.push(id);
        }
    }

    let mut capabilities = direct_capabilities_from_ids(&ids, kind);
    if let Some(info) = direct_feature_info_with_timeout(
        device,
        slot,
        HiResWheelFeature::ID,
        "Root getFeature HiResWheel",
        DIRECT_CAPABILITY_TIMEOUT,
    )
    .await
    {
        ids.push(HiResWheelFeature::ID);
        let feature = HiResWheelFeature::new(Arc::clone(channel), slot, info.index);
        capabilities.scroll_inversion = optional_read_with_timeout(
            slot,
            "HiResWheel getWheelCapabilities",
            feature.get_wheel_capabilities(),
            DIRECT_CAPABILITY_TIMEOUT,
        )
        .await
        .is_some_and(|wheel| wheel.has_invert);
    }
    let measured_capabilities = Capabilities::from_feature_ids(&ids);
    let direct_peripheral_evidence = battery.is_some()
        || model_info
            .as_ref()
            .is_some_and(DeviceModelInfo::has_model_identity)
        || measured_capabilities.buttons
        || measured_capabilities.pointer
        || measured_capabilities.lighting;

    debug!(
        slot,
        feature_count = ids.len(),
        feature_ids = %format_feature_ids(&ids),
        battery_index,
        capabilities = ?capabilities,
        direct_peripheral_evidence,
        "direct root probe completed"
    );

    (
        ProbedFeatures {
            battery,
            model_info,
            kind,
            capabilities: Some(capabilities),
            direct_peripheral_evidence,
        },
        battery_index,
    )
}

async fn direct_feature_info(
    device: &Device,
    slot: u8,
    id: u16,
    what: &'static str,
) -> Option<FeatureInformation> {
    direct_feature_info_with_timeout(device, slot, id, what, OPTIONAL_FEATURE_TIMEOUT).await
}

async fn direct_feature_info_with_timeout(
    device: &Device,
    slot: u8,
    id: u16,
    what: &'static str,
    budget: Duration,
) -> Option<FeatureInformation> {
    let root = device.root();
    optional_read_with_timeout(slot, what, root.get_feature(id), budget)
        .await
        .flatten()
}

fn direct_capability_probe_ids(kind: Option<DeviceKind>) -> &'static [u16] {
    match kind {
        Some(DeviceKind::Mouse | DeviceKind::Trackball | DeviceKind::Touchpad) => {
            DIRECT_POINTER_CAPABILITY_FEATURE_IDS
        }
        Some(DeviceKind::Keyboard | DeviceKind::Numpad) => DIRECT_KEYBOARD_CAPABILITY_FEATURE_IDS,
        _ => DIRECT_CAPABILITY_FEATURE_IDS,
    }
}

fn direct_capabilities_from_ids(ids: &[u16], kind: Option<DeviceKind>) -> Capabilities {
    let mut capabilities = Capabilities::from_feature_ids(ids);
    if let Some(kind) = kind {
        let presumed = Capabilities::presumed_from_kind(kind);
        capabilities.buttons |= presumed.buttons;
        capabilities.pointer |= presumed.pointer;
        capabilities.lighting |= presumed.lighting;
    }
    capabilities
}

async fn optional_read<T, E>(
    slot: u8,
    what: &'static str,
    read: impl Future<Output = Result<T, E>>,
) -> Option<T>
where
    E: std::fmt::Debug,
{
    optional_read_with_timeout(slot, what, read, OPTIONAL_FEATURE_TIMEOUT).await
}

async fn optional_read_with_timeout<T, E>(
    slot: u8,
    what: &'static str,
    read: impl Future<Output = Result<T, E>>,
    budget: Duration,
) -> Option<T>
where
    E: std::fmt::Debug,
{
    match timeout(budget, read).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(e)) => {
            debug!(slot, feature = what, error = ?e, "optional HID++ feature read failed");
            None
        }
        Err(_) => {
            debug!(
                slot,
                feature = what,
                budget = ?budget,
                "optional HID++ feature read timed out"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hidpp::feature::{CreatableFeature as _, unified_battery::UnifiedBatteryFeature};
    use openlogi_core::device::DeviceKind;

    use super::{
        DIRECT_CAPABILITY_FEATURE_IDS, DIRECT_CAPABILITY_TIMEOUT,
        DIRECT_KEYBOARD_CAPABILITY_FEATURE_IDS, DIRECT_POINTER_CAPABILITY_FEATURE_IDS,
        OPTIONAL_FEATURE_TIMEOUT, battery_feature_index, direct_capabilities_from_ids,
        direct_capability_probe_ids, format_feature_ids,
    };

    #[test]
    fn optional_feature_timeout_is_bounded_inside_probe_budget() {
        assert!(
            OPTIONAL_FEATURE_TIMEOUT >= Duration::from_secs(1),
            "BLE direct optional reads need enough room for a slow macOS report callback"
        );
        assert!(
            OPTIONAL_FEATURE_TIMEOUT < super::super::PROBE_BUDGET,
            "optional reads must not consume the whole node probe budget"
        );
        assert!(
            DIRECT_CAPABILITY_TIMEOUT * (DIRECT_CAPABILITY_FEATURE_IDS.len() as u32)
                < super::super::PROBE_BUDGET,
            "direct capability probes are best-effort and must not exhaust the whole probe budget"
        );
    }

    #[test]
    fn direct_probe_capability_ids_cover_ui_panels() {
        let caps =
            openlogi_core::device::Capabilities::from_feature_ids(DIRECT_CAPABILITY_FEATURE_IDS);
        assert!(caps.buttons, "direct root probe must check button features");
        assert!(
            caps.pointer,
            "direct root probe must check pointer features"
        );
        assert!(
            caps.lighting,
            "direct root probe must check lighting features"
        );
    }

    #[test]
    fn direct_probe_limits_capability_queries_by_device_kind() {
        assert_eq!(
            direct_capability_probe_ids(Some(DeviceKind::Mouse)),
            DIRECT_POINTER_CAPABILITY_FEATURE_IDS
        );
        assert_eq!(
            direct_capability_probe_ids(Some(DeviceKind::Keyboard)),
            DIRECT_KEYBOARD_CAPABILITY_FEATURE_IDS
        );
        assert_eq!(
            direct_capability_probe_ids(None),
            DIRECT_CAPABILITY_FEATURE_IDS
        );
    }

    #[test]
    fn direct_probe_presumes_mouse_panels_when_capability_reads_time_out() {
        let caps = direct_capabilities_from_ids(&[], Some(DeviceKind::Mouse));
        assert!(caps.buttons);
        assert!(caps.pointer);
        assert!(!caps.lighting);
    }

    #[test]
    fn battery_index_is_one_based_in_the_enumerated_table() {
        // `enumerate_features` omits the root feature (index 0), so the first
        // enumerated entry sits at runtime index 1.
        let table = [0x0001, UnifiedBatteryFeature::ID, 0x2201];
        assert_eq!(battery_feature_index(table), Some(2));
        assert_eq!(
            battery_feature_index([UnifiedBatteryFeature::ID]),
            Some(1),
            "first entry maps to index 1, not 0"
        );
    }

    #[test]
    fn no_battery_feature_means_no_index() {
        assert_eq!(battery_feature_index([0x0001, 0x2201, 0x1b04]), None);
        assert_eq!(battery_feature_index([]), None);
    }

    #[test]
    fn feature_trace_ids_are_hex_formatted() {
        assert_eq!(
            format_feature_ids(&[0x0001, 0x1004, 0x2202]),
            "0x0001, 0x1004, 0x2202"
        );
        assert_eq!(format_feature_ids(&[]), "");
    }
}
