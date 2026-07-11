use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature, device_information::DeviceInformationFeature,
        device_type_and_name::DeviceTypeAndNameFeature, hires_wheel::HiResWheelFeature,
        thumbwheel::ThumbwheelFeature, unified_battery::UnifiedBatteryFeature,
    },
};
use openlogi_core::device::{
    BatteryInfo, Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports,
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
    if let Some(caps) = capabilities.as_mut()
        && let Some(feature) = device.get_feature::<ThumbwheelFeature>()
    {
        match feature.get_thumbwheel_info().await {
            Ok(info) => {
                caps.thumbwheel_tap = info.capabilities.single_tap;
                debug!(
                    slot,
                    supported = caps.thumbwheel_tap,
                    "thumb wheel single-tap capability probed"
                );
            }
            Err(e) => debug!(slot, error = ?e, "thumb wheel capability probe failed"),
        }
    }

    let battery = match battery_index {
        Some(feature_index) => read_battery(channel, slot, feature_index).await,
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
    let kind = match device.get_feature::<DeviceTypeAndNameFeature>() {
        Some(feature) => match feature.get_device_type().await {
            Ok(ty) => Some(map_device_type(ty)),
            Err(e) => {
                debug!(slot, error = ?e, "DeviceType read failed");
                None
            }
        },
        None => None,
    };

    (
        ProbedFeatures {
            battery,
            model_info,
            kind,
            capabilities,
        },
        battery_index,
    )
}

#[cfg(test)]
mod tests {
    use hidpp::feature::{CreatableFeature as _, unified_battery::UnifiedBatteryFeature};

    use super::battery_feature_index;

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
}
