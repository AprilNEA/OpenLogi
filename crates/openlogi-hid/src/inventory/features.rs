use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::hires_wheel::HiResWheelFeature,
    feature::{
        CreatableFeature, battery_status::BatteryStatus as LegacyBatteryStatus,
        battery_status::BatteryStatusFeature, device_information::DeviceInformationFeature,
        device_type_and_name::DeviceTypeAndNameFeature, unified_battery::UnifiedBatteryFeature,
    },
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, BatteryStatus, Capabilities, DeviceKind, DeviceModelInfo,
    DeviceTransports,
};
use tracing::debug;

use crate::mappings::{
    map_battery_level, map_battery_status, map_device_type, normalize_serial_number,
};

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
    /// Marketing name from `0x0005`, used when the receiver codename is absent.
    pub(super) marketing_name: Option<String>,
}

/// Runtime feature-table address used for cheap battery refreshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BatteryFeatureIndex {
    Unified(u8),
    Legacy(u8),
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

async fn read_legacy_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    feature_index: u8,
) -> Option<BatteryInfo> {
    let feature = BatteryStatusFeature::new(Arc::clone(channel), slot, feature_index);
    feature.get_battery_level_status().await.ok().map(|info| {
        let level = battery_level_from_percentage(info.percentage);
        let status = match info.status {
            LegacyBatteryStatus::Discharging => BatteryStatus::Discharging,
            LegacyBatteryStatus::Charging | LegacyBatteryStatus::ChargingNearlyFull => {
                BatteryStatus::Charging
            }
            LegacyBatteryStatus::Full => BatteryStatus::Full,
            LegacyBatteryStatus::ChargingSlow => BatteryStatus::ChargingSlow,
            LegacyBatteryStatus::InvalidBattery
            | LegacyBatteryStatus::ThermalError
            | LegacyBatteryStatus::ChargingError => BatteryStatus::Error,
            _ => BatteryStatus::Unknown,
        };
        BatteryInfo {
            percentage: info.percentage,
            level,
            status,
        }
    })
}

fn battery_level_from_percentage(percentage: u8) -> BatteryLevel {
    match percentage {
        100 => BatteryLevel::Full,
        20..=99 => BatteryLevel::Good,
        5..=19 => BatteryLevel::Low,
        _ => BatteryLevel::Critical,
    }
}

pub(super) async fn read_battery_at(
    channel: &Arc<HidppChannel>,
    slot: u8,
    index: BatteryFeatureIndex,
) -> Option<BatteryInfo> {
    match index {
        BatteryFeatureIndex::Unified(index) => read_battery(channel, slot, index).await,
        BatteryFeatureIndex::Legacy(index) => read_legacy_battery(channel, slot, index).await,
    }
}

/// Runtime index of the `UnifiedBattery` feature in an enumerated feature-ID
/// table, for [`read_battery`]. The table is 1-based (index 0 is the implicit
/// root feature, which enumeration omits).
pub(super) fn battery_feature_index(
    ids: impl IntoIterator<Item = u16>,
) -> Option<BatteryFeatureIndex> {
    let ids = ids.into_iter().collect::<Vec<_>>();
    let index_of = |feature_id| {
        ids.iter()
            .position(|id| *id == feature_id)
            .and_then(|pos| u8::try_from(pos + 1).ok())
    };
    index_of(UnifiedBatteryFeature::ID)
        .map(BatteryFeatureIndex::Unified)
        .or_else(|| index_of(BatteryStatusFeature::ID).map(BatteryFeatureIndex::Legacy))
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
) -> (ProbedFeatures, Option<BatteryFeatureIndex>) {
    let mut device = match Device::new(Arc::clone(channel), slot).await {
        Ok(d) => d,
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            return (ProbedFeatures::default(), None);
        }
    };
    // The enumeration response IS the device's feature-ID table — capture it
    // for capability derivation instead of discarding it.
    let mut battery_index = None;
    let mut capabilities = match device.enumerate_features().await {
        Ok(Some(features)) => {
            let ids: Vec<u16> = features.iter().map(|f| f.id).collect();
            battery_index = battery_feature_index(ids.iter().copied());
            Some(Capabilities::from_feature_ids(&ids))
        }
        Ok(None) => None,
        Err(e) => {
            debug!(slot, error = ?e, "enumerate_features failed");
            return (ProbedFeatures::default(), None);
        }
    };
    if let Some(caps) = capabilities.as_mut()
        && let Some(feature) = device.get_feature::<HiResWheelFeature>()
    {
        caps.scroll_inversion = feature
            .get_wheel_capabilities()
            .await
            .is_ok_and(|wheel| wheel.has_invert);
    }

    let battery = match battery_index {
        Some(feature_index) => read_battery_at(channel, slot, feature_index).await,
        None => None,
    };

    let model_info = match device.get_feature::<DeviceInformationFeature>() {
        Some(feature) => match feature.get_device_info().await {
            Ok(info) => {
                let serial_number = if info.capabilities.serial_number {
                    match feature.get_serial_number().await {
                        Ok(serial) => normalize_serial_number(&serial),
                        Err(e) => {
                            debug!(slot, error = ?e, "DeviceInformation serial read failed");
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
                        usb: info.transport.usb,
                        equad: info.transport.e_quad,
                        btle: info.transport.btle,
                        bluetooth: info.transport.bluetooth,
                    },
                    model_ids: info.model_id,
                    extended_model_id: info.extended_model_id,
                })
            }
            Err(e) => {
                debug!(slot, error = ?e, "DeviceInformation read failed");
                None
            }
        },
        None => None,
    };

    // `0x0005` reports the device's own marketing type (mouse, keyboard, …) —
    // the authoritative kind signal. On the direct path it's the only one; on
    // the Bolt path it corrects a pairing register that reported the wrong (or
    // `Unknown`) kind.
    let (kind, marketing_name) = match device.get_feature::<DeviceTypeAndNameFeature>() {
        Some(feature) => {
            let kind = match feature.get_device_type().await {
                Ok(ty) => Some(map_device_type(ty)),
                Err(e) => {
                    debug!(slot, error = ?e, "DeviceType read failed");
                    None
                }
            };
            let name = feature
                .get_whole_device_name()
                .await
                .ok()
                .filter(|name| !name.trim().is_empty());
            (kind, name)
        }
        None => (None, None),
    };

    (
        ProbedFeatures {
            battery,
            model_info,
            kind,
            capabilities,
            marketing_name,
        },
        battery_index,
    )
}

#[cfg(test)]
mod tests {
    use hidpp::feature::{CreatableFeature as _, unified_battery::UnifiedBatteryFeature};

    use super::{BatteryFeatureIndex, battery_feature_index, battery_level_from_percentage};
    use openlogi_core::device::BatteryLevel;

    #[test]
    fn battery_index_is_one_based_in_the_enumerated_table() {
        // `enumerate_features` omits the root feature (index 0), so the first
        // enumerated entry sits at runtime index 1.
        let table = [0x0001, UnifiedBatteryFeature::ID, 0x2201];
        assert_eq!(
            battery_feature_index(table),
            Some(BatteryFeatureIndex::Unified(2))
        );
        assert_eq!(
            battery_feature_index([UnifiedBatteryFeature::ID]),
            Some(BatteryFeatureIndex::Unified(1)),
            "first entry maps to index 1, not 0"
        );
    }

    #[test]
    fn no_battery_feature_means_no_index() {
        assert_eq!(battery_feature_index([0x0001, 0x2201, 0x1b04]), None);
        assert_eq!(battery_feature_index([]), None);
    }

    #[test]
    fn legacy_battery_is_used_when_unified_is_absent() {
        assert_eq!(
            battery_feature_index([0x0001, 0x1000, 0x2201]),
            Some(BatteryFeatureIndex::Legacy(2))
        );
        assert_eq!(
            battery_feature_index([0x1000, 0x1004]),
            Some(BatteryFeatureIndex::Unified(2)),
            "0x1004 is preferred when both generations are advertised"
        );
    }

    #[test]
    fn legacy_percentage_maps_to_openlogi_levels() {
        assert_eq!(battery_level_from_percentage(100), BatteryLevel::Full);
        assert_eq!(battery_level_from_percentage(99), BatteryLevel::Good);
        assert_eq!(battery_level_from_percentage(20), BatteryLevel::Good);
        assert_eq!(battery_level_from_percentage(19), BatteryLevel::Low);
        assert_eq!(battery_level_from_percentage(5), BatteryLevel::Low);
        assert_eq!(battery_level_from_percentage(4), BatteryLevel::Critical);
        assert_eq!(battery_level_from_percentage(0), BatteryLevel::Critical);
    }
}
