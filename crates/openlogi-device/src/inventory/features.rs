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
        multi_platform::{MultiPlatformCapabilities, MultiPlatformFeature},
        reprog_controls::{ReprogControlsFeature, control_ids},
        unified_battery::UnifiedBatteryFeature,
    },
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::reprog_controls::{BACK_CIDS, DPI_MODE_SHIFT_CIDS, FORWARD_CIDS};

use super::events::{EventFeatureIndices, EventSubscriptionHandle};
use super::mappings::{
    legacy_battery_info, legacy_battery_level_from_percentage, map_adc_battery_status,
    map_battery_level, map_battery_status, map_device_type, map_voltage_battery_status,
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
    /// `battery`, so never persisted. The cache resolves both fields from
    /// [`BatteryRead`]; `true` implies `battery` is `None`.
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

/// Which battery feature a device exposes plus its runtime feature index. Newer
/// devices answer the unified `0x1004`; MX2S-era ones only the legacy `0x1000`
/// — the same enhanced-then-legacy split SmartShift has with `0x2111`/`0x2110`.
/// G-series wireless gaming devices (G915, G903 LS) expose neither and report
/// battery only as a voltage via `0x1001`; wireless G-series headsets (G733)
/// report it as a voltage via `0x1F20` instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum BatteryProbe {
    /// `0x1004` at `index`. `state_of_charge` is the device's capability bit
    /// for a percentage: without it the status reply's percentage byte is not a
    /// reading (always zero) and the device reports only a coarse level.
    Unified {
        index: u8,
        state_of_charge: bool,
    },
    Legacy(u8),
    Voltage(u8),
    Adc(u8),
}

/// Read just the battery by addressing its feature at the known runtime index —
/// one round-trip, with no `Device::new` ping and no feature-table walk. This is
/// the cache's battery read after a full probe or on a cache hit. The cache
/// resolves an unavailable answer against its last-known state.
pub(super) async fn read_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: BatteryProbe,
) -> BatteryRead {
    let reading = match probe {
        BatteryProbe::Unified {
            index,
            state_of_charge,
        } => {
            let feature = UnifiedBatteryFeature::new(Arc::clone(channel), slot, index);
            feature
                .get_battery_info()
                .await
                .ok()
                .map(|info| BatteryInfo {
                    percentage: state_of_charge.then_some(info.charging_percentage),
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
                .map(|info| legacy_battery_info(info.discharge_level, info.status))
        }
        BatteryProbe::Voltage(feature_index) => {
            let feature = BatteryVoltageFeature::new(Arc::clone(channel), slot, feature_index);
            feature.get_battery_info().await.ok().map(|info| {
                let percentage = voltage_battery_percentage(info.voltage_mv);
                BatteryInfo {
                    percentage: Some(percentage),
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
                        percentage: Some(percentage),
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
///
/// The table alone cannot say whether a `0x1004` reports a percentage, so a
/// unified source comes back assuming it does; [`probe_features`] then reads the
/// capability.
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
            return Some(BatteryProbe::Unified {
                index,
                state_of_charge: true,
            });
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

/// Read device identity and capabilities through one HID++ session for `slot`.
/// Reuse the enumerated feature table to derive [`Capabilities`] without
/// another round-trip.
///
/// Return the battery feature separately. The cache owns the live read and
/// resolves the reading against its last-known state after this probe.
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

    let (battery_probe, state_of_charge_unknown) =
        resolve_state_of_charge(channel, slot, battery_probe).await;
    capabilities_incomplete |= state_of_charge_unknown;
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
            battery: None,
            model_info,
            kind,
            marketing_name,
            capabilities,
            identity_incomplete,
            capabilities_incomplete,
            unlinked: false,
        },
        battery_probe,
        event_features,
    )
}

/// Read whether a `0x1004` battery gauges its state of charge, and return the
/// probe with that answer. Other probes pass through unchanged.
///
/// The capability is fixed per device, so it is read once at probe time and
/// memoized with the probe rather than on every battery read. The flag is
/// `true` when the read failed: the probe then keeps the assumed percentage
/// for this read, and the caller marks the probe incomplete so that guess is
/// not memoized and the next reconciliation re-probes.
async fn resolve_state_of_charge(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: Option<BatteryProbe>,
) -> (Option<BatteryProbe>, bool) {
    let Some(BatteryProbe::Unified { index, .. }) = probe else {
        return (probe, false);
    };
    match UnifiedBatteryFeature::new(Arc::clone(channel), slot, index)
        .get_battery_capabilities()
        .await
    {
        Ok(caps) => (
            Some(BatteryProbe::Unified {
                index,
                state_of_charge: caps.percentage,
            }),
            false,
        ),
        Err(e) => {
            debug!(slot, error = ?e, "UnifiedBattery capability read failed");
            (probe, true)
        }
    }
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
    // Logi Options+ writes a keyboard's platform only when `0x4531` lets
    // software set it, so a keyboard that does not is offered no setting.
    if caps.os_layout
        && let Some(feature) = device.get_feature::<MultiPlatformFeature>()
    {
        caps.os_layout = feature
            .get_feature_infos()
            .await
            .map_err(|_| ())?
            .capabilities
            .contains(MultiPlatformCapabilities::SET_HOST_PLATFORM);
    }
    if let Some(feature) = device.get_feature::<ReprogControlsFeature>() {
        let count = feature.get_count().await.map_err(|_| ())?;
        let mut haptic_panel = false;
        let mut dpi_gestures = false;
        let mut virtual_thumbwheel = false;
        for index in 0..count {
            let info = feature.get_cid_info(index).await.map_err(|_| ())?;
            haptic_panel |= probe_haptic_controls
                && info.cid == control_ids::HAPTIC_PANEL
                && info.flags.is_divertable();
            dpi_gestures |= DPI_MODE_SHIFT_CIDS.contains(&info.cid.0)
                && info.flags.is_divertable()
                && info.flags.supports_raw_xy();
            // The same Back/Forward families arming diverts.
            virtual_thumbwheel |= (BACK_CIDS.contains(&info.cid.0)
                || FORWARD_CIDS.contains(&info.cid.0))
                && info.flags.is_divertable()
                && info.flags.supports_raw_wheel();
        }
        // Publish only a complete control walk. A lost reply must retain the
        // cache's last-good capabilities and schedule repair, not hide support.
        caps.haptic_panel = haptic_panel;
        caps.dpi_gestures = dpi_gestures;
        caps.virtual_thumbwheel = virtual_thumbwheel;
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

    use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};

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
    async fn virtual_thumbwheel_needs_back_or_forward_divertable_with_raw_wheel() {
        // A raw-wheel middle button is a decoy: only Back/Forward hold the wheel.
        for cid in [0x0053, 0x00bd, 0x00ce, 0x00db, 0x0056, 0x00cf, 0x0052] {
            for (flags, supported) in [(0x0820, true), (0x0020, false), (0x0800, false)] {
                let probe = control_probe(
                    vec![0x0001, 0x1b04],
                    vec![(0x0052, 0x0820), (cid, flags)],
                    None,
                )
                .await;
                let caps = probe.capabilities.unwrap();
                assert!(!probe.capabilities_incomplete);
                assert_eq!(
                    caps.virtual_thumbwheel,
                    supported && cid != 0x0052,
                    "CID {cid:04x}, flags {flags:04x}"
                );
            }
        }
    }

    #[tokio::test]
    async fn control_walk_publishes_its_capabilities_only_after_all_rows_succeed() {
        for fail_at in [Some(0), Some(1), Some(2), None] {
            let probe = control_probe(
                vec![0x0001, 0x1b04, 0x19b0],
                vec![(0x01a0, 0x0020), (0x00ed, 0x0120), (0x0056, 0x0820)],
                fail_at,
            )
            .await;
            let caps = probe.capabilities.unwrap();
            assert_eq!(probe.capabilities_incomplete, fail_at.is_some());
            assert_eq!(caps.haptic_panel, fail_at.is_none());
            assert_eq!(caps.dpi_gestures, fail_at.is_none());
            assert_eq!(caps.virtual_thumbwheel, fail_at.is_none());
        }
    }

    /// Probe a keyboard whose `0x4531` (at index 2) reports the capability
    /// word `capabilities`, or fails the read when it is `None`.
    async fn multi_platform_probe(capabilities: Option<u16>) -> ProbedFeatures {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            let features = [0x0001_u16, 0x4531];
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 1,
                (1, 0) => response[4] = 2,
                (1, 1) => {
                    let id = features[usize::from(request[4]) - 1];
                    response[4..6].copy_from_slice(&id.to_be_bytes());
                    // `MultiPlatformFeature` binds from version 1.
                    response[7] = u8::from(id == 0x4531);
                }
                (2, 0) => match capabilities {
                    Some(word) => response[4..6].copy_from_slice(&word.to_be_bytes()),
                    None => return Some(feature_error(request, 0x08)),
                },
                _ => panic!("unexpected capability request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        probe_features(&channel, 0xff, None).await.0
    }

    #[tokio::test]
    async fn os_layout_needs_a_keyboard_that_lets_software_set_its_platform() {
        for (word, settable) in [
            (0x0200, true),
            (0x0300, true),
            (0x0100, false),
            (0x0002, false),
        ] {
            let probe = multi_platform_probe(Some(word)).await;
            assert!(!probe.capabilities_incomplete);
            assert_eq!(
                probe.capabilities.unwrap().os_layout,
                settable,
                "capabilities {word:04x}"
            );
        }
        // An unanswered read leaves the set incomplete, so it is not memoized.
        let probe = multi_platform_probe(None).await;
        assert!(probe.capabilities_incomplete);
    }

    /// A `0x1004` source as the feature table alone describes it, before the
    /// probe reads its capability.
    const fn unified(index: u8) -> BatteryProbe {
        BatteryProbe::Unified {
            index,
            state_of_charge: true,
        }
    }

    /// `0x1004` capability flags: bit 0 rechargeable, bit 1 state of charge.
    const RECHARGEABLE: u8 = 1;
    const STATE_OF_CHARGE: u8 = 1 << 1;

    /// Probe a device whose only battery source is a `0x1004` at index 2, then
    /// read the battery through the probe the way the cache does. `flags` is
    /// the capability reply's flags byte (`None` fails the read); `status` is
    /// the `getStatus` reply's percentage, level and charging bytes.
    async fn unified_probe(
        flags: Option<u8>,
        status: [u8; 3],
    ) -> (ProbedFeatures, Option<BatteryProbe>, Option<BatteryInfo>) {
        let features = [0x0001, UnifiedBatteryFeature::ID];
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
                (2, 0) => {
                    let Some(flags) = flags else {
                        return Some(feature_error(request, 0x08));
                    };
                    response[4] = 0x0f;
                    response[5] = flags;
                }
                (2, 1) => response[4..7].copy_from_slice(&status),
                _ => panic!("unexpected request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let (probe, battery_probe, _) = probe_features(&channel, 0xff, None).await;
        let battery = match battery_probe {
            Some(battery_probe) => match read_battery(&channel, 0xff, battery_probe).await {
                BatteryRead::Reading(info) => Some(info),
                _ => None,
            },
            None => None,
        };
        (probe, battery_probe, battery)
    }

    #[tokio::test]
    async fn a_unified_battery_without_state_of_charge_reports_only_its_level() {
        // Level "good", discharging; the percentage byte is zero because the
        // device has no gauge, not because the battery is empty.
        let (probe, battery_probe, battery) = unified_probe(Some(RECHARGEABLE), [0, 0x04, 0]).await;
        assert_eq!(
            battery_probe,
            Some(BatteryProbe::Unified {
                index: 2,
                state_of_charge: false,
            })
        );
        assert!(!probe.capabilities_incomplete);
        let battery = battery.expect("a level-only reading");
        assert_eq!(battery.percentage, None);
        assert_eq!(battery.level, BatteryLevel::Good);
        assert_eq!(battery.status, BatteryStatus::Discharging);
    }

    #[tokio::test]
    async fn a_unified_battery_with_state_of_charge_reports_its_percentage() {
        let (probe, battery_probe, battery) =
            unified_probe(Some(RECHARGEABLE | STATE_OF_CHARGE), [67, 0x04, 0]).await;
        assert_eq!(battery_probe, Some(unified(2)));
        assert!(!probe.capabilities_incomplete);
        assert_eq!(battery.expect("a reading").percentage, Some(67));
    }

    #[tokio::test]
    async fn a_failed_capability_read_keeps_the_percentage_and_reprobes() {
        let (probe, battery_probe, battery) = unified_probe(None, [67, 0x04, 0]).await;
        assert_eq!(battery_probe, Some(unified(2)));
        assert!(
            probe.capabilities_incomplete,
            "an assumed capability must not be memoized"
        );
        assert_eq!(battery.expect("a reading").percentage, Some(67));
    }

    #[test]
    fn battery_index_is_one_based_in_the_enumerated_table() {
        // `enumerate_features` omits the root feature (index 0), so the first
        // enumerated entry sits at runtime index 1.
        let table = [0x0001, UnifiedBatteryFeature::ID, 0x2201];
        assert_eq!(battery_feature_index(table), Some(unified(2)));
        assert_eq!(
            battery_feature_index([UnifiedBatteryFeature::ID]),
            Some(unified(1)),
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
        assert_eq!(battery_feature_index(table), Some(unified(3)));
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
        assert_eq!(battery_feature_index(table), Some(unified(2)));
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
        assert_eq!(battery.percentage, Some(67));
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

    /// Reads the battery through a scripted `0x1000` at feature index 2 whose
    /// `getBatteryLevelStatus` answers `[discharge_level, next_level, status]`.
    async fn legacy_battery(wire: [u8; 3]) -> BatteryInfo {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            assert_eq!(
                (request[2], request[3] >> 4),
                (2, 0),
                "unexpected request: {request:02x?}"
            );
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            response[4..7].copy_from_slice(&wire);
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let BatteryRead::Reading(battery) =
            read_battery(&channel, 0xff, BatteryProbe::Legacy(2)).await
        else {
            panic!("expected a reading");
        };
        battery
    }

    #[tokio::test]
    async fn legacy_battery_reads_the_level_while_discharging() {
        let battery = legacy_battery([50, 20, 0]).await;
        assert_eq!(battery.percentage, Some(50));
        assert_eq!(battery.level, BatteryLevel::Good);
        assert_eq!(battery.status, BatteryStatus::Discharging);
    }

    #[tokio::test]
    async fn legacy_charge_complete_reads_full_not_the_unknown_zero() {
        // Off battery the firmware reports level 0, meaning unknown.
        let battery = legacy_battery([0, 0, 3]).await;
        assert_eq!(battery.percentage, Some(100));
        assert_eq!(battery.level, BatteryLevel::Full);
        assert_eq!(battery.status, BatteryStatus::Full);
    }

    #[tokio::test]
    async fn legacy_charging_zero_is_no_reading_not_critical() {
        for status in [1, 2, 4] {
            let battery = legacy_battery([0, 0, status]).await;
            assert_eq!(battery.percentage, None, "status {status}");
            assert_eq!(battery.level, BatteryLevel::Unknown, "status {status}");
        }
    }

    #[test]
    fn adc_battery_ranks_below_every_other_source() {
        for (other, expected) in [
            (UnifiedBatteryFeature::ID, unified(2)),
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
