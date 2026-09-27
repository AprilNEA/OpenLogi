use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::hires_wheel::HiResWheelFeature,
    feature::{
        CreatableFeature,
        adc_measurement::{AdcMeasurement, AdcMeasurementFeature},
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

use crate::reprog_controls::DPI_MODE_SHIFT_CIDS;

use super::events::{EventFeatureIndices, EventSubscriptionHandle};
use super::mappings::{
    legacy_battery_level_from_percentage, map_adc_battery_status, map_battery_level,
    map_battery_status, map_device_type, map_legacy_battery_status, map_voltage_battery_status,
    normalize_serial_number, voltage_battery_percentage,
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
    /// The battery feature answered that the battery-powered end is not
    /// linked: a headset dongle whose headset is switched off. Volatile like
    /// `battery`, so never persisted. Invariant: `true` implies `battery` is
    /// `None`; the only writers are [`probe_features`] and the cache's battery
    /// re-read, both of which take the pair from one [`BatteryRead`].
    #[serde(skip)]
    pub(super) unlinked: bool,
}

/// What one battery read learned about the device.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BatteryRead {
    /// A live reading.
    Reading(BatteryInfo),
    /// The battery-powered end is not linked (see [`ProbedFeatures::unlinked`]).
    Unlinked,
    /// No usable answer — asleep, mid-host-switch, a timeout. Says nothing
    /// about presence.
    Unavailable,
}

impl BatteryRead {
    /// The `(battery, unlinked)` pair a probe records for this read.
    pub(super) fn into_parts(self) -> (Option<BatteryInfo>, bool) {
        match self {
            Self::Reading(battery) => (Some(battery), false),
            Self::Unlinked => (None, true),
            Self::Unavailable => (None, false),
        }
    }
}

/// Which battery feature a device exposes plus its runtime feature index. Newer
/// devices answer the unified `0x1004`; MX2S-era ones only the legacy `0x1000`
/// — the same enhanced-then-legacy split SmartShift has with `0x2111`/`0x2110`.
/// G-series wireless gaming devices (G915, G903 LS) expose neither and report
/// battery only as a voltage via `0x1001`; wireless G-series headsets (G733)
/// report it as a voltage via `0x1F20` instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum BatteryProbe {
    Unified(u8),
    Legacy(u8),
    Voltage(u8),
    Adc(u8),
}

/// Read just the battery by addressing its feature at the known runtime index —
/// one round-trip, with no `Device::new` ping and no feature-table walk. This is
/// both the full probe's battery read (the walk just produced the index) and the
/// cheap per-reconciliation refresh for cache hits.
pub(super) async fn read_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: BatteryProbe,
) -> BatteryRead {
    let reading = match probe {
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
        BatteryProbe::Adc(feature_index) => {
            let feature = AdcMeasurementFeature::new(Arc::clone(channel), slot, feature_index);
            match feature.get_adc_measurement().await {
                Ok(AdcMeasurement::Linked { voltage_mv, status }) => {
                    let percentage = voltage_battery_percentage(voltage_mv);
                    Some(BatteryInfo {
                        percentage,
                        level: legacy_battery_level_from_percentage(percentage),
                        status: map_adc_battery_status(status),
                    })
                }
                Ok(AdcMeasurement::Unlinked) => return BatteryRead::Unlinked,
                _ => None,
            }
        }
    };
    reading.map_or(BatteryRead::Unavailable, BatteryRead::Reading)
}

/// Locate a device's battery feature in an enumerated feature-ID table,
/// preferring the unified `0x1004`, then the legacy `0x1000`, then the
/// voltage-only `0x1001` and `0x1F20` (which report no percentage, so a direct
/// source always outranks them). The table is 1-based (index 0 is the implicit root
/// feature, which enumeration omits).
pub(super) fn battery_feature_index(ids: impl IntoIterator<Item = u16>) -> Option<BatteryProbe> {
    // A feature table holds at most `u8::MAX` entries (its count is a u8), so a
    // 1-based index always fits.
    let mut legacy = None;
    let mut voltage = None;
    let mut adc = None;
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
        if id == AdcMeasurementFeature::ID && adc.is_none() {
            adc = Some(BatteryProbe::Adc(index));
        }
    }
    legacy.or(voltage).or(adc)
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

    let (battery, unlinked) = match battery_probe {
        Some(probe) => read_battery(channel, slot, probe).await.into_parts(),
        None => (None, false),
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
            unlinked,
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
    if let Some(feature) = device.get_feature::<ReprogControlsFeature>() {
        let count = feature.get_count().await.map_err(|_| ())?;
        let mut haptic_panel = false;
        let mut dpi_gestures = false;
        for index in 0..count {
            let info = feature.get_cid_info(index).await.map_err(|_| ())?;
            haptic_panel |= probe_haptic_controls
                && info.cid == control_ids::HAPTIC_PANEL
                && info.flags.is_divertable();
            dpi_gestures |= DPI_MODE_SHIFT_CIDS.contains(&info.cid.0)
                && info.flags.is_divertable()
                && info.flags.supports_raw_xy();
        }
        // Publish only a complete control walk. A lost reply must retain the
        // cache's last-good capabilities and schedule repair, not hide support.
        caps.haptic_panel = haptic_panel;
        caps.dpi_gestures = dpi_gestures;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use hidpp::feature::{
        CreatableFeature as _, adc_measurement::AdcMeasurementFeature,
        battery_status::BatteryStatusFeature, battery_voltage::BatteryVoltageFeature,
        unified_battery::UnifiedBatteryFeature,
    };

    use openlogi_core::device::{BatteryLevel, BatteryStatus};

    use super::{
        BatteryProbe, BatteryRead, ProbedFeatures, battery_feature_index, probe_features,
        read_battery,
    };
    use crate::channel::scripted::{ScriptedRawHidChannel, feature_error, scripted_channel};

    async fn control_probe(
        features: Vec<u16>,
        controls: Vec<(u16, u16)>,
        fail_at: Option<u8>,
    ) -> ProbedFeatures {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 1,
                (1, 0) => response[4] = u8::try_from(features.len()).unwrap(),
                (1, 1) => response[4..6]
                    .copy_from_slice(&features[usize::from(request[4]) - 1].to_be_bytes()),
                (2, 0) => response[4] = u8::try_from(controls.len()).unwrap(),
                (2, 1) => {
                    if fail_at == Some(request[4]) {
                        return Some(feature_error(request, 0x08));
                    }
                    let (cid, flags) = controls[usize::from(request[4])];
                    response[4..6].copy_from_slice(&cid.to_be_bytes());
                    let [low, high] = flags.to_le_bytes();
                    response[8] = low;
                    response[12] = high;
                }
                _ => panic!("unexpected capability request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        probe_features(&channel, 0xff, None).await.0
    }

    #[tokio::test]
    async fn dpi_gestures_require_a_matching_divertable_raw_xy_control() {
        // A raw-XY gesture button is a decoy: only DPI-family support counts.
        for cid in [0x00c4, 0x00ed, 0x00fd, 0x0053] {
            for (flags, supported) in [(0x0120, true), (0x0020, false), (0x0100, false), (0, false)]
            {
                let probe = control_probe(
                    vec![0x0001, 0x1b04],
                    vec![(0x00c3, 0x0120), (cid, flags)],
                    None,
                )
                .await;
                let caps = probe.capabilities.unwrap();
                assert!(!probe.capabilities_incomplete);
                assert_eq!(
                    caps.dpi_gestures,
                    supported && cid != 0x0053,
                    "CID {cid:04x}, flags {flags:04x}"
                );
                assert!(!caps.haptic_panel);
            }
        }
    }

    #[tokio::test]
    async fn control_walk_publishes_both_capabilities_only_after_all_rows_succeed() {
        for fail_at in [Some(0), Some(1), None] {
            let probe = control_probe(
                vec![0x0001, 0x1b04, 0x19b0],
                vec![(0x01a0, 0x0020), (0x00ed, 0x0120)],
                fail_at,
            )
            .await;
            let caps = probe.capabilities.unwrap();
            assert_eq!(probe.capabilities_incomplete, fail_at.is_some());
            assert_eq!(caps.haptic_panel, fail_at.is_none());
            assert_eq!(caps.dpi_gestures, fail_at.is_none());
        }
    }

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
    fn adc_battery_is_found_when_it_is_the_only_source() {
        // The G733 feature table: 0x1F20 is its only battery source, at 8.
        let table = [
            0x0001, 0x0003, 0x0005, 0x8070, 0x8010, 0x8310, 0x8300, 0x1f20,
        ];
        assert_eq!(AdcMeasurementFeature::ID, 0x1f20);
        assert_eq!(battery_feature_index(table), Some(BatteryProbe::Adc(8)));
    }

    /// Reads the battery through a scripted `0x1F20` at feature index 8 whose
    /// `getAdcMeasurement` answers `wire`, or the HID++ error code `wire[0]`
    /// when `error` is set.
    async fn adc_battery(wire: [u8; 3], error: bool) -> BatteryRead {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            assert_eq!(
                (request[2], request[3] >> 4),
                (8, 0),
                "unexpected request: {request:02x?}"
            );
            if error {
                return Some(feature_error(request, wire[0]));
            }
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            response[4..7].copy_from_slice(&wire);
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        read_battery(&channel, 0xff, BatteryProbe::Adc(8)).await
    }

    #[tokio::test]
    async fn adc_battery_estimates_a_percentage_from_the_voltage() {
        // The G733's live answer: 3905 mV, linked, on battery.
        let BatteryRead::Reading(battery) = adc_battery([0x0f, 0x41, 0x01], false).await else {
            panic!("expected a reading");
        };
        assert_eq!(battery.percentage, 67);
        assert_eq!(battery.level, BatteryLevel::Good);
        assert_eq!(battery.status, BatteryStatus::Discharging);

        let BatteryRead::Reading(charging) = adc_battery([0x10, 0x04, 0x03], false).await else {
            panic!("expected a reading");
        };
        assert_eq!(charging.status, BatteryStatus::Charging);
    }

    #[tokio::test]
    async fn adc_battery_tells_unlinked_from_unavailable() {
        // G733 headset switched off: its dongle answers error 0x05, captured
        // from hardware.
        assert_eq!(adc_battery([0x05, 0, 0], true).await, BatteryRead::Unlinked);
        // Link bit clear (Solaar and the kernel read it as device inactive).
        assert_eq!(
            adc_battery([0x00, 0x00, 0x00], false).await,
            BatteryRead::Unlinked
        );
        // The kernel's explicit "unknown" flags value is no reading, not a
        // switched-off device.
        assert_eq!(
            adc_battery([0x0f, 0x41, 0x0f], false).await,
            BatteryRead::Unavailable
        );
        // Neither is a busy device.
        assert_eq!(
            adc_battery([0x08, 0, 0], true).await,
            BatteryRead::Unavailable
        );
    }

    #[test]
    fn adc_battery_ranks_below_every_other_source() {
        for (other, expected) in [
            (UnifiedBatteryFeature::ID, BatteryProbe::Unified(2)),
            (BatteryStatusFeature::ID, BatteryProbe::Legacy(2)),
            (BatteryVoltageFeature::ID, BatteryProbe::Voltage(2)),
        ] {
            let table = [AdcMeasurementFeature::ID, other];
            assert_eq!(battery_feature_index(table), Some(expected), "{other:#06x}");
        }
    }

    #[test]
    fn no_battery_feature_means_no_index() {
        assert_eq!(battery_feature_index([0x0001, 0x2201, 0x1b04]), None);
        assert_eq!(battery_feature_index([]), None);
    }
}
