use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::hires_wheel::HiResWheelFeature,
    feature::{
        CreatableFeature,
        battery_status::BatteryStatusFeature,
        battery_voltage::BatteryVoltageFeature,
        device_information::{DeviceInformationFeature, DeviceTransport},
        device_type_and_name::DeviceTypeAndNameFeature,
        gestures2::Gestures2Feature,
        reprog_controls::{ReprogControlsFeature, control_ids},
        unified_battery::UnifiedBatteryFeature,
    },
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::events::{EventFeatureIndices, EventSubscriptionHandle};
use super::mappings::{
    legacy_battery_level_from_percentage, map_battery_level, map_battery_status, map_device_type,
    map_legacy_battery_status, map_voltage_battery_status, normalize_serial_number,
    voltage_battery_percentage,
};

/// Everything a single device probe yields. Any field is `None` when the
/// device doesn't expose that feature or the read failed.
#[derive(Default, Clone, Serialize, Deserialize)]
pub(super) struct ProbedFeatures {
    pub(super) battery: Option<BatteryInfo>,
    pub(super) model_info: Option<DeviceModelInfo>,
    /// Marketing type from HID++ `0x0005` — an identity hint only.
    pub(super) kind: Option<DeviceKind>,
    /// Marketing name from HID++ `0x0005`; preferred over generic OS HID names
    /// such as Windows Bluetooth's plain `"Mouse"`.
    pub(super) marketing_name: Option<String>,
    /// Configuration capabilities derived from the device's feature table.
    ///
    /// Invariant: `capabilities_incomplete` implies this is `Some`. The sole
    /// non-test writer, [`probe_features`], returns `Default` when the feature
    /// table is unavailable and only sets the qualifier after constructing the
    /// capability set. The cache only replaces that set with another `Some`,
    /// and persistence only round-trips probes produced here. If another writer
    /// is added, preserve this proof or replace the pair with a sum type.
    pub(super) capabilities: Option<Capabilities>,
    /// A `DeviceInformation` read *failed* (vs. the feature being absent), so
    /// the identity fields above may be missing data the device does have.
    /// This is intentionally independent of `model_info`: `true` with `Some`
    /// means only the serial-number read failed, while `true` with `None` means
    /// the whole device-information read failed.
    pub(super) identity_incomplete: bool,
    /// A capability read *failed* (vs. the device not having the capability),
    /// so `capabilities` above understates what the device can do. Memoizing
    /// that would hide a panel in the GUI for `REFRESH_INTERVAL`.
    pub(super) capabilities_incomplete: bool,
}

/// Which battery feature a device exposes plus its runtime feature index. Newer
/// devices answer the unified `0x1004`; MX2S-era ones only the legacy `0x1000`
/// — the same enhanced-then-legacy split SmartShift has with `0x2111`/`0x2110`.
/// G-series wireless gaming devices (G915, G903 LS) expose neither and report
/// battery only as a voltage via `0x1001`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum BatteryProbe {
    Unified(u8),
    Legacy(u8),
    Voltage(u8),
}

/// Read just the battery by addressing its feature at the known runtime index —
/// one round-trip, with no `Device::new` ping and no feature-table walk. This is
/// both the full probe's battery read (the walk just produced the index) and the
/// cheap per-reconciliation refresh for cache hits. `None` when the device
/// doesn't answer (asleep, switched hosts).
pub(super) async fn read_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: BatteryProbe,
) -> Option<BatteryInfo> {
    match probe {
        BatteryProbe::Unified(feature_index) => {
            let feature = UnifiedBatteryFeature::new(Arc::clone(channel), slot, feature_index);
            feature
                .get_battery_info()
                .await
                .ok()
                .map(|info| BatteryInfo {
                    percentage: info.charging_percentage,
                    level: map_battery_level(info.level),
                    status: map_battery_status(info.status),
                })
        }
        BatteryProbe::Legacy(feature_index) => {
            let feature = BatteryStatusFeature::new(Arc::clone(channel), slot, feature_index);
            feature
                .get_battery_level_status()
                .await
                .ok()
                .map(|info| BatteryInfo {
                    percentage: info.discharge_level,
                    level: legacy_battery_level_from_percentage(info.discharge_level),
                    status: map_legacy_battery_status(info.status),
                })
        }
        BatteryProbe::Voltage(feature_index) => {
            let feature = BatteryVoltageFeature::new(Arc::clone(channel), slot, feature_index);
            feature.get_battery_info().await.ok().map(|info| {
                let percentage = voltage_battery_percentage(info.voltage_mv);
                BatteryInfo {
                    percentage,
                    // The firmware's own critical marker outranks our
                    // estimated bucket.
                    level: if info.critical {
                        BatteryLevel::Critical
                    } else {
                        legacy_battery_level_from_percentage(percentage)
                    },
                    status: map_voltage_battery_status(info.status),
                }
            })
        }
    }
}

/// Locate a device's battery feature in an enumerated feature-ID table,
/// preferring the unified `0x1004`, then the legacy `0x1000`, then the
/// voltage-only `0x1001` (which reports no percentage, so a direct source
/// always outranks it). The table is 1-based (index 0 is the implicit root
/// feature, which enumeration omits).
pub(super) fn battery_feature_index(ids: impl IntoIterator<Item = u16>) -> Option<BatteryProbe> {
    // A feature table holds at most `u8::MAX` entries (its count is a u8), so a
    // 1-based index always fits.
    let mut legacy = None;
    let mut voltage = None;
    for (pos, id) in ids.into_iter().enumerate() {
        // Stop gracefully past u8::MAX instead of `?`-returning None, which would
        // discard a `legacy` already found. (The table caps at 255, so unreachable.)
        let Ok(index) = u8::try_from(pos + 1) else {
            break;
        };
        if id == UnifiedBatteryFeature::ID {
            return Some(BatteryProbe::Unified(index));
        }
        if id == BatteryStatusFeature::ID && legacy.is_none() {
            legacy = Some(BatteryProbe::Legacy(index));
        }
        if id == BatteryVoltageFeature::ID && voltage.is_none() {
            voltage = Some(BatteryProbe::Voltage(index));
        }
    }
    legacy.or(voltage)
}

/// Read the marketing identity from HID++ `0x0005` when the device exposes it.
async fn read_marketing_identity(
    device: &Device,
    slot: u8,
) -> (Option<DeviceKind>, Option<String>) {
    let Some(feature) = device.get_feature::<DeviceTypeAndNameFeature>() else {
        return (None, None);
    };

    let kind = match feature.get_device_type().await {
        Ok(ty) => Some(map_device_type(ty)),
        Err(e) => {
            debug!(slot, error = ?e, "DeviceType read failed");
            None
        }
    };
    let name = match feature.get_whole_device_name().await {
        Ok(name) if !name.trim().is_empty() => Some(name),
        Ok(_) => None,
        Err(e) => {
            debug!(slot, error = ?e, "DeviceName read failed");
            None
        }
    };
    (kind, name)
}

/// Open a HID++ session for `slot` and read everything we care about (battery,
/// device-information, `0x0005` device type, and the feature table that drives
/// [`Capabilities`]) in one shot. Device sessions are expensive (multi-round-
/// trip) so we fold every read through the same `Device::new` +
/// `enumerate_features` — the feature table is the Vec that enumeration already
/// returns, so capabilities cost no extra round-trip.
///
/// Also returns the battery feature found by the walk, so later ticks can
/// refresh the battery without repeating it.
///
/// Only online, responsive devices reach here.
pub(super) async fn probe_features(
    channel: &Arc<HidppChannel>,
    slot: u8,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> (ProbedFeatures, Option<BatteryProbe>, EventFeatureIndices) {
    let mut device = match Device::new(Arc::clone(channel), slot).await {
        Ok(d) => d,
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            return (
                ProbedFeatures::default(),
                None,
                EventFeatureIndices::default(),
            );
        }
    };
    // The enumeration response IS the device's feature-ID table — capture it
    // for capability derivation instead of discarding it.
    let mut battery_probe = None;
    let mut event_features = EventFeatureIndices::default();
    let mut probe_haptic_controls = false;
    let mut capabilities = match device.enumerate_features().await {
        Ok(Some(features)) => {
            let ids: Vec<u16> = features.iter().map(|f| f.id).collect();
            battery_probe = battery_feature_index(ids.iter().copied());
            event_features = EventFeatureIndices::from_feature_ids(&ids);
            if let Some(subscriptions) = subscriptions {
                // Register immediately after the table read, before the
                // battery/identity snapshot that will be published.
                subscriptions.register_device(slot, event_features);
            }
            probe_haptic_controls = ids.contains(&0x19b0) || ids.contains(&0x19c0);
            Some(Capabilities::from_feature_ids(&ids))
        }
        Ok(None) => None,
        Err(e) => {
            debug!(slot, error = ?e, "enumerate_features failed");
            return (
                ProbedFeatures::default(),
                None,
                EventFeatureIndices::default(),
            );
        }
    };
    let mut capabilities_incomplete = false;
    if let Some(caps) = capabilities.as_mut() {
        capabilities_incomplete = probe_extra_capabilities(&device, caps, probe_haptic_controls)
            .await
            .is_err();
    }

    let battery = match battery_probe {
        Some(probe) => read_battery(channel, slot, probe).await,
        None => None,
    };

    let mut identity_incomplete = false;
    let model_info = match device.get_feature::<DeviceInformationFeature>() {
        Some(feature) => match feature.get_device_info().await {
            Ok(info) => {
                let serial_number = if info.capabilities.serial_number {
                    match feature.get_serial_number().await {
                        Ok(serial) => normalize_serial_number(&serial),
                        Err(e) => {
                            debug!(slot, error = ?e, "DeviceInformation serial read failed");
                            identity_incomplete = true;
                            None
                        }
                    }
                } else {
                    None
                };
                Some(DeviceModelInfo {
                    entity_count: info.entity_count,
                    serial_number,
                    unit_id: info.unit_id,
                    transports: DeviceTransports {
                        usb: info.transport.contains(DeviceTransport::USB),
                        equad: info.transport.contains(DeviceTransport::E_QUAD),
                        btle: info.transport.contains(DeviceTransport::BTLE),
                        bluetooth: info.transport.contains(DeviceTransport::BLUETOOTH),
                    },
                    model_ids: info.model_id,
                    extended_model_id: info.extended_model_id,
                })
            }
            Err(e) => {
                debug!(slot, error = ?e, "DeviceInformation read failed");
                identity_incomplete = true;
                None
            }
        },
        None => None,
    };

    // `0x0005` reports the device's own marketing type and name. The type is
    // the authoritative kind signal; the marketing name matters especially on
    // Windows Bluetooth, where the OS HID collection is often just `"Mouse"`.
    let (kind, marketing_name) = read_marketing_identity(&device, slot).await;

    (
        ProbedFeatures {
            battery,
            model_info,
            kind,
            marketing_name,
            capabilities,
            identity_incomplete,
            capabilities_incomplete,
        },
        battery_probe,
        event_features,
    )
}

/// Fill in the capabilities the feature table alone can't answer, each of which
/// costs its own round-trips.
///
/// `Err(())` means a read failed, so the set now understates the device — the
/// caller must not let that be memoized. A capability whose read merely says
/// "no" is not an error: only an unanswered read is.
async fn probe_extra_capabilities(
    device: &Device,
    caps: &mut Capabilities,
    probe_haptic_controls: bool,
) -> Result<(), ()> {
    if let Some(feature) = device.get_feature::<HiResWheelFeature>() {
        caps.scroll_inversion = feature
            .get_wheel_capabilities()
            .await
            .is_ok_and(|wheel| wheel.has_invert);
    }
    // Older MX mice (notably MX Master 2S) expose the horizontal wheel as
    // Gestures2 gesture id 46 instead of the newer dedicated 0x2150
    // Thumbwheel feature. Inspect the descriptor table so a generic 0x6501
    // touch device does not become a false-positive thumbwheel device.
    if !caps.thumbwheel
        && let Some(feature) = device.get_feature::<Gestures2Feature>()
    {
        caps.thumbwheel = feature.has_thumbwheel().await.unwrap_or(false);
    }
    if probe_haptic_controls && let Some(feature) = device.get_feature::<ReprogControlsFeature>() {
        match has_haptic_panel(&feature).await {
            Some(found) => caps.haptic_panel = found,
            None => return Err(()),
        }
    }
    Ok(())
}

/// Whether the device exposes a divertable haptic panel, or `None` when a read
/// failed part-way through the ~40-entry control walk.
///
/// The distinction matters because the answer is memoized for `REFRESH_INTERVAL`:
/// reporting a lost reply as `false` hides the Actions Ring binding for half a
/// minute on a device that has the panel.
async fn has_haptic_panel(feature: &ReprogControlsFeature) -> Option<bool> {
    let count = feature.get_count().await.ok()?;
    for index in 0..count {
        let info = feature.get_cid_info(index).await.ok()?;
        if info.cid == control_ids::HAPTIC_PANEL {
            return Some(info.flags.is_divertable());
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use hidpp::feature::{
        CreatableFeature as _, battery_status::BatteryStatusFeature,
        battery_voltage::BatteryVoltageFeature, unified_battery::UnifiedBatteryFeature,
    };

    use super::{BatteryProbe, battery_feature_index};

    #[test]
    fn battery_index_is_one_based_in_the_enumerated_table() {
        // `enumerate_features` omits the root feature (index 0), so the first
        // enumerated entry sits at runtime index 1.
        let table = [0x0001, UnifiedBatteryFeature::ID, 0x2201];
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Unified(2)));
        assert_eq!(
            battery_feature_index([UnifiedBatteryFeature::ID]),
            Some(BatteryProbe::Unified(1)),
            "first entry maps to index 1, not 0"
        );
    }

    #[test]
    fn legacy_battery_is_found_when_unified_is_absent() {
        let table = [0x0001, BatteryStatusFeature::ID, 0x2201];
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Legacy(2)));
    }

    #[test]
    fn unified_battery_is_preferred_over_legacy() {
        let table = [BatteryStatusFeature::ID, 0x0001, UnifiedBatteryFeature::ID];
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Unified(3)));
    }

    #[test]
    fn voltage_battery_is_found_when_it_is_the_only_source() {
        // The G915 / G903 LS case: 0x1001 with neither 0x1000 nor 0x1004.
        let table = [0x0001, BatteryVoltageFeature::ID, 0x2201];
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Voltage(2)));
    }

    #[test]
    fn direct_percentage_sources_outrank_the_voltage_estimate() {
        let table = [BatteryVoltageFeature::ID, BatteryStatusFeature::ID];
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Legacy(2)));
        let table = [BatteryVoltageFeature::ID, UnifiedBatteryFeature::ID];
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Unified(2)));
    }

    #[test]
    fn no_battery_feature_means_no_index() {
        assert_eq!(battery_feature_index([0x0001, 0x2201, 0x1b04]), None);
        assert_eq!(battery_feature_index([]), None);
    }
}
