use std::sync::Arc;
use std::time::Duration;

use hidpp::{
    channel::HidppChannel,
    device::{Device, DeviceError},
    feature::hires_wheel::HiResWheelFeature,
    feature::{
        CreatableFeature,
        adc_measurement::{AdcMeasurement, AdcMeasurementFeature},
        battery_status::BatteryStatusFeature,
        battery_voltage::BatteryVoltageFeature,
        device_information::{DeviceInformationFeature, DeviceTransport},
        device_type_and_name::DeviceTypeAndNameFeature,
        disable_keys::DisableKeysFeature,
        force_sensing_button::ForceSensingButtonFeature,
        gestures2::Gestures2Feature,
        multi_platform::{MultiPlatformCapabilities, MultiPlatformFeature},
        reprog_controls::{ReprogControlsFeature, control_ids},
        unified_battery::UnifiedBatteryFeature,
    },
    protocol::{
        v10::Hidpp10Error,
        v20::{ErrorType, Hidpp20Error},
    },
    register::battery::{self as register_battery, BatteryRegister},
};
use openlogi_core::device::{
    BatteryInfo, BatteryLevel, Capabilities, DeviceKind, DeviceModelInfo, DeviceTransports,
};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tracing::debug;

use crate::host_lock::ReceiverRegisterPhase;
use crate::reprog_controls::{BACK_CIDS, DPI_MODE_SHIFT_CIDS, FORWARD_CIDS};
use crate::write::{force_button_is_changeable, panel_button};

use super::events::{EventFeatureIndices, EventSubscriptionHandle};
use super::mappings::{
    charge_register_battery_info, legacy_battery_info, legacy_battery_level_from_percentage,
    map_adc_battery_status, map_battery_level, map_battery_status, map_device_type,
    map_voltage_battery_status, normalize_serial_number, status_register_battery_info,
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
    /// Which protocol the device answered in, and with it the configuration
    /// capabilities derived from its feature table.
    pub(super) protocol: ProbedProtocol,
    /// A `DeviceInformation` read *failed* (vs. the feature being absent), so
    /// the identity fields above may be missing data the device does have.
    /// This is intentionally independent of `model_info`: `true` with `Some`
    /// means only the serial-number read failed, while `true` with `None` means
    /// the whole device-information read failed.
    pub(super) identity_incomplete: bool,
    /// The battery feature answered that the battery-powered end is not
    /// linked: a headset dongle whose headset is switched off. Volatile like
    /// `battery`, so never persisted. The cache resolves both fields from
    /// [`BatteryRead`]; `true` implies `battery` is `None`.
    #[serde(skip)]
    pub(super) unlinked: bool,
    /// Whether these fields answer for the device now. Only the cache marks a
    /// stand-in, so this is never persisted.
    #[serde(skip)]
    pub(super) refresh: Refresh,
}

/// Whether a probe's data is the device's answer or the cache's stand-in for
/// a device that did not answer its refresh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Refresh {
    /// The data comes from a probe the device completed: this one, or the one
    /// behind a cache entry that is still fresh, or whose due refresh the
    /// device answered without finishing the re-read (see
    /// [`refresh_features`]).
    #[default]
    Answered,
    /// The device did not answer a due refresh's ping, so the cache's
    /// last-known answer stands in. Callers keep publishing its identity, but
    /// must not take it as evidence that the device is reachable.
    Failed,
}

impl ProbedFeatures {
    /// The capabilities a completed HID++ 2.0 feature walk measured. `None`
    /// for a HID++ 1.0 device too: it has no table to measure, so the UI keeps
    /// its kind-based fallback.
    pub(super) fn capabilities(&self) -> Option<Capabilities> {
        match self.protocol {
            ProbedProtocol::Hidpp20 { capabilities, .. } => Some(capabilities),
            ProbedProtocol::Unanswered | ProbedProtocol::Hidpp10 => None,
        }
    }

    /// Whether a capability read failed part way, so [`Self::capabilities`]
    /// understates the device. Only a HID++ 2.0 feature walk can fall short
    /// this way.
    pub(super) fn capabilities_incomplete(&self) -> bool {
        matches!(
            self.protocol,
            ProbedProtocol::Hidpp20 {
                incomplete: true,
                ..
            }
        )
    }

    /// Whether the probe learned what the device is: a completed HID++ 2.0
    /// feature walk, or a HID++ 1.0 device, which has no table to walk. Only a
    /// completed probe is memoized or followed by a receiver codename read.
    pub(super) fn completed(&self) -> bool {
        !matches!(self.protocol, ProbedProtocol::Unanswered)
    }
}

/// What a probe learned about the protocol a device speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum ProbedProtocol {
    /// The probe did not get that far: the device never answered, or its
    /// feature table could not be read.
    #[default]
    Unanswered,
    /// HID++ 2.0, with the capabilities derived from its feature table.
    Hidpp20 {
        capabilities: Capabilities,
        /// A capability read *failed* (vs. the device not having the
        /// capability), so `capabilities` understates what the device can do.
        /// Memoizing that would hide a panel in the GUI for
        /// `REFRESH_INTERVAL`.
        incomplete: bool,
    },
    /// HID++ 1.0: no feature table, so no capabilities to derive. The
    /// receiver supplies the device's name.
    Hidpp10,
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
/// report it as a voltage via `0x1F20` instead. HID++ 1.0 devices (Performance
/// MX, M705) have no feature table and report it through a fixed register.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum BatteryProbe {
    /// `0x1004` at `index`. `state_of_charge` is the device's capability bit
    /// for a percentage: without it the status reply's percentage byte is not a
    /// reading (always zero) and the device reports only a coarse level.
    Unified {
        index: u8,
        state_of_charge: bool,
    },
    /// `0x1000` at `index`. `mileage` is the device's capability for a gauge
    /// reading: without it the discharge level encodes one of four coarse
    /// levels, which take the draft's ranges instead of the display buckets.
    Legacy {
        index: u8,
        mileage: bool,
    },
    Voltage(u8),
    Adc(u8),
    /// HID++ 1.0 register `0x07`: a coarse level, no percentage.
    StatusRegister,
    /// HID++ 1.0 register `0x0D`: a percentage.
    ChargeRegister,
}

/// Read just the battery by addressing its feature at the known runtime index,
/// or its HID++ 1.0 register — one round-trip, with no `Device::new` ping and
/// no feature-table walk. This is the cache's battery read after a full probe
/// or on a cache hit. The cache resolves an unavailable answer against its
/// last-known state.
///
/// A HID++ 1.0 register is read only under the receiver's register phase,
/// for the reason `probe_hidpp10` gives; without `registers` it is
/// unavailable.
pub(super) async fn read_battery(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: BatteryProbe,
    registers: Option<&ReceiverRegisterPhase>,
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
        BatteryProbe::Legacy { index, mileage } => {
            let feature = BatteryStatusFeature::new(Arc::clone(channel), slot, index);
            feature
                .get_battery_level_status()
                .await
                .ok()
                .map(|info| legacy_battery_info(info.discharge_level, info.status, mileage))
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
        BatteryProbe::StatusRegister | BatteryProbe::ChargeRegister if registers.is_none() => None,
        BatteryProbe::StatusRegister => register_battery::read_status(channel, slot)
            .await
            .ok()
            .map(status_register_battery_info),
        BatteryProbe::ChargeRegister => register_battery::read_charge(channel, slot)
            .await
            .ok()
            .map(charge_register_battery_info),
    };
    reading.map_or(BatteryRead::Unavailable, BatteryRead::Reading)
}

/// Locate a device's battery feature in an enumerated feature-ID table,
/// preferring the unified `0x1004`, then the legacy `0x1000`, then the
/// voltage-only `0x1001` and `0x1F20` (which report no percentage, so a direct
/// source always outranks them). The table is 1-based (index 0 is the implicit root
/// feature, which enumeration omits).
///
/// The table alone cannot say whether a `0x1004` reports a percentage, or
/// whether a `0x1000` reports mileage, so either source comes back assuming
/// it does; [`probe_features`] then reads the capability.
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
            legacy = Some(BatteryProbe::Legacy {
                index,
                mileage: true,
            });
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

fn derive_capabilities(
    features: &[hidpp::feature::feature_set::FeatureInformation],
    ids: &[u16],
) -> Capabilities {
    let mut capabilities = Capabilities::from_feature_ids(ids);
    capabilities.disable_keys = features.iter().any(|feature| {
        feature.id == DisableKeysFeature::ID && feature.typ.is_end_user_configurable()
    });
    capabilities
}

/// Read device identity and capabilities through one HID++ session for `slot`,
/// in whichever protocol the device answers.
///
/// Return the battery feature separately. The cache owns the live read and
/// resolves the reading against its last-known state after this probe.
///
/// `registers` is the receiver's register phase, when the caller holds it for
/// the whole probe. A HID++ 1.0 device is probed only under it (see
/// `probe_hidpp10`); without it the device stays unanswered.
///
/// Only online, responsive devices reach here.
pub(super) async fn probe_features(
    channel: &Arc<HidppChannel>,
    slot: u8,
    subscriptions: Option<&EventSubscriptionHandle>,
    registers: Option<&ReceiverRegisterPhase>,
) -> FeatureProbe {
    match Device::new(Arc::clone(channel), slot).await {
        Ok(device) => probe_hidpp20(device, channel, slot, subscriptions).await,
        Err(DeviceError::UnsupportedProtocolVersion) => {
            probe_hidpp10_held(channel, slot, registers).await
        }
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            unanswered()
        }
    }
}

/// What one device probe yields: its features, the battery source the cache
/// reads, and the event-capable feature indexes.
pub(super) type FeatureProbe = (ProbedFeatures, Option<BatteryProbe>, EventFeatureIndices);

/// What a due refresh of a device the cache already knows learned.
pub(super) enum Refreshed {
    /// The device did not answer the protocol ping, so nothing shows that it
    /// is reachable.
    Unreachable,
    /// The device answered the ping, so its channel delivers replies. The
    /// probe after the ping is `None` when it ran past its budget.
    Answered(Option<FeatureProbe>),
}

/// [`probe_features`] for a device the cache already knows, with the work
/// after the protocol ping bounded by `walk_budget`.
///
/// The ping shows whether the device is reachable. The walk after it re-reads
/// immutable data that the cache already holds, with one request per feature
/// and per reprogrammable control among others. A control-table read is not
/// retried. A Bluetooth link that drops a few replies can make the walk outlast
/// the whole probe budget while the device keeps answering (#2107). A probe
/// cut off by that budget counts as a failure, and two failures retire the
/// channel. With its own budget, the walk ends first, and the caller still
/// learns that the device answered.
pub(super) async fn refresh_features(
    channel: &Arc<HidppChannel>,
    slot: u8,
    subscriptions: Option<&EventSubscriptionHandle>,
    registers: Option<&ReceiverRegisterPhase>,
    walk_budget: Duration,
) -> Refreshed {
    let walk = match Device::new(Arc::clone(channel), slot).await {
        Ok(device) => {
            timeout(
                walk_budget,
                probe_hidpp20(device, channel, slot, subscriptions),
            )
            .await
        }
        Err(DeviceError::UnsupportedProtocolVersion) => {
            timeout(walk_budget, probe_hidpp10_held(channel, slot, registers)).await
        }
        Err(e) => {
            debug!(slot, error = ?e, "Device::new failed");
            return Refreshed::Unreachable;
        }
    };
    if walk.is_err() {
        debug!(
            slot,
            ?walk_budget,
            "refresh walk ran past its budget after the device answered its ping"
        );
    }
    Refreshed::Answered(walk.ok())
}

/// [`probe_hidpp10`] when the caller holds the receiver's register phase.
/// Without it, the device stays unanswered.
async fn probe_hidpp10_held(
    channel: &HidppChannel,
    slot: u8,
    registers: Option<&ReceiverRegisterPhase>,
) -> FeatureProbe {
    if let Some(registers) = registers {
        probe_hidpp10(channel, slot, registers).await
    } else {
        debug!(
            slot,
            "HID++ 1.0 device outside a register phase, not probed"
        );
        unanswered()
    }
}

/// The probe of a device that gave no usable answer.
fn unanswered() -> FeatureProbe {
    (
        ProbedFeatures::default(),
        None,
        EventFeatureIndices::default(),
    )
}

/// Probe a HID++ 2.0 device. Reuse the enumerated feature table to derive
/// [`Capabilities`] without another round-trip.
async fn probe_hidpp20(
    mut device: Device,
    channel: &Arc<HidppChannel>,
    slot: u8,
    subscriptions: Option<&EventSubscriptionHandle>,
) -> (ProbedFeatures, Option<BatteryProbe>, EventFeatureIndices) {
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
            Some(derive_capabilities(&features, &ids))
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

    let (battery_probe, battery_capability_unknown) =
        resolve_battery_capability(channel, slot, battery_probe).await;
    capabilities_incomplete |= battery_capability_unknown;
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
            protocol: capabilities.map_or(ProbedProtocol::Unanswered, |capabilities| {
                ProbedProtocol::Hidpp20 {
                    capabilities,
                    incomplete: capabilities_incomplete,
                }
            }),
            identity_incomplete,
            unlinked: false,
            refresh: Refresh::Answered,
        },
        battery_probe,
        event_features,
    )
}

/// Probe a HID++ 1.0 device. With no feature table there are no capabilities
/// or device-side identity to read, and the receiver supplies its name and
/// kind; what is left to learn is which register reports its battery.
///
/// These are register reads, which carry no software id, so another OpenLogi
/// process could take their replies. They go out only under the receiver's
/// register phase, which `_registers` proves the caller holds. The Unifying
/// slot walk holds it to its end; the direct path and the Bolt slot walk run
/// outside it.
async fn probe_hidpp10(
    channel: &HidppChannel,
    slot: u8,
    _registers: &ReceiverRegisterPhase,
) -> (ProbedFeatures, Option<BatteryProbe>, EventFeatureIndices) {
    match find_battery_register(channel, slot).await {
        Ok(battery) => (
            ProbedFeatures {
                protocol: ProbedProtocol::Hidpp10,
                ..ProbedFeatures::default()
            },
            battery,
            EventFeatureIndices::default(),
        ),
        Err(e) => {
            debug!(slot, error = ?e, "HID++ 1.0 battery-register probe failed");
            (
                ProbedFeatures::default(),
                None,
                EventFeatureIndices::default(),
            )
        }
    }
}

/// The battery register a HID++ 1.0 device implements, trying `0x0D` before
/// `0x07` as Solaar does; `Ok(None)` when it refuses both. A read the device
/// did not answer is an `Err`, not an absent register, so the probe is retried
/// instead of memoizing a device without a battery.
async fn find_battery_register(
    channel: &HidppChannel,
    slot: u8,
) -> Result<Option<BatteryProbe>, Hidpp10Error> {
    for (register, probe) in [
        (BatteryRegister::Charge, BatteryProbe::ChargeRegister),
        (BatteryRegister::Status, BatteryProbe::StatusRegister),
    ] {
        if register.is_implemented(channel, slot).await? {
            return Ok(Some(probe));
        }
    }
    Ok(None)
}

/// Read the battery capability the feature table cannot answer (see
/// [`battery_feature_index`]) and return the probe with that answer. The flag
/// is `true` when the read failed.
async fn resolve_battery_capability(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: Option<BatteryProbe>,
) -> (Option<BatteryProbe>, bool) {
    let (probe, state_of_charge_unknown) = resolve_state_of_charge(channel, slot, probe).await;
    let (probe, mileage_unknown) = resolve_legacy_mileage(channel, slot, probe).await;
    (probe, state_of_charge_unknown || mileage_unknown)
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

/// Read whether a legacy `0x1000` battery reports mileage, and return the
/// probe with that answer. Other probes pass through unchanged.
///
/// Memoized with the probe for the same reason as
/// [`resolve_state_of_charge`], with the same failure rule: a failed read
/// keeps the assumed mileage (the display buckets) for this read, and the flag
/// tells the caller to mark the probe incomplete.
async fn resolve_legacy_mileage(
    channel: &Arc<HidppChannel>,
    slot: u8,
    probe: Option<BatteryProbe>,
) -> (Option<BatteryProbe>, bool) {
    let Some(BatteryProbe::Legacy { index, .. }) = probe else {
        return (probe, false);
    };
    match BatteryStatusFeature::new(Arc::clone(channel), slot, index)
        .get_battery_capability()
        .await
    {
        Ok(capability) => (
            Some(BatteryProbe::Legacy {
                index,
                mileage: capability.reports_mileage(),
            }),
            false,
        ),
        Err(e) => {
            debug!(slot, error = ?e, "BatteryStatus capability read failed");
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
    // An unanswered 0x19c0 read keeps the feature table's presumption and
    // marks the set incomplete, but does not skip the control walk below.
    let mut force_sensing_read = Ok(());
    if caps.force_sensing
        && let Some(feature) = device.get_feature::<ForceSensingButtonFeature>()
    {
        match probe_force_sensing(&feature).await {
            Ok(changeable) => caps.force_sensing = changeable,
            Err(()) => force_sensing_read = Err(()),
        }
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
    force_sensing_read
}

/// Whether the device lets software change the [`panel_button`]'s press
/// force; the write path refuses a button that does not.
///
/// `Err(())` means a read went unanswered: the channel failed or the device
/// was busy. Any other reply, including a feature error, is an answer.
async fn probe_force_sensing(feature: &ForceSensingButtonFeature) -> Result<bool, ()> {
    match feature.get_button_count().await {
        Ok(0) => Ok(false),
        Ok(_) => match panel_button(feature).await {
            Ok(button) => Ok(force_button_is_changeable(&button)),
            Err(e) => force_sensing_error_answer(e),
        },
        Err(e) => force_sensing_error_answer(e),
    }
}

/// Classify a failed `0x19c0` read: `Ok(false)` when the device answered it,
/// `Err(())` when it went unanswered.
fn force_sensing_error_answer(error: Hidpp20Error) -> Result<bool, ()> {
    match error {
        Hidpp20Error::Feature(ErrorType::Busy) => {
            debug!("ForceSensingButton read refused: device busy");
            Err(())
        }
        // `UnsupportedResponse` covers a short reply, one naming no usable
        // range or a default outside it, and an error code this crate does
        // not know; either way the device answered.
        Hidpp20Error::Feature(_) | Hidpp20Error::UnsupportedResponse => Ok(false),
        error => {
            debug!(error = ?error, "ForceSensingButton read went unanswered");
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hidpp::feature::{
        CreatableFeature as _, FeatureType, adc_measurement::AdcMeasurementFeature,
        battery_status::BatteryStatusFeature, battery_voltage::BatteryVoltageFeature,
        disable_keys::DisableKeysFeature, unified_battery::UnifiedBatteryFeature,
    };

    use openlogi_core::device::{BatteryInfo, BatteryLevel, BatteryStatus};

    use super::{
        BatteryProbe, BatteryRead, ProbedFeatures, ProbedProtocol, Refreshed,
        battery_feature_index, probe_features, read_battery, refresh_features,
    };
    use crate::channel::scripted::{
        ScriptedRawHidChannel, feature_error, ping_reply, register_error, scripted_channel,
    };
    use crate::host_lock::ReceiverRegisterPhase;

    #[tokio::test]
    async fn a_refresh_reports_the_answered_ping_when_its_walk_runs_past_the_budget() {
        // Only the ping is answered. The feature-table lookup after it never
        // is, so the walk can only end at its budget.
        let (raw, _) = ScriptedRawHidChannel::with_responder(ping_reply);
        let channel = scripted_channel(raw).await;
        let refreshed =
            refresh_features(&channel, 0xff, None, None, Duration::from_millis(50)).await;
        assert!(
            matches!(refreshed, Refreshed::Answered(None)),
            "the ping was answered, and the walk did not finish"
        );
    }

    #[tokio::test]
    async fn a_refresh_whose_ping_goes_unanswered_is_unreachable() {
        let (raw, _) = ScriptedRawHidChannel::with_failing_writes(|_| None, |_| true);
        let channel = scripted_channel(raw).await;
        let refreshed = refresh_features(&channel, 0xff, None, None, Duration::from_secs(8)).await;
        assert!(matches!(refreshed, Refreshed::Unreachable));
    }

    /// A HID++ 1.0 mouse at slot 1 with register `0x07` (a Performance MX, per
    /// Solaar), whose read of register `0x0D` answers `charge_register_error`.
    async fn hidpp10_probe(charge_register_error: u8) -> (ProbedFeatures, Option<BatteryProbe>) {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            match (request[2], request[3]) {
                // HID++ 2.0's ping reaches a HID++ 1.0 device as sub id 0x00.
                (0x00, _) => Some(register_error(request, 0x01)),
                (0x81, 0x0d) => Some(register_error(request, charge_register_error)),
                (0x81, 0x07) => Some(vec![0x10, request[1], 0x81, 0x07, 0x05, 0x00, 0x00]),
                _ => panic!("unexpected request: {request:02x?}"),
            }
        });
        let channel = scripted_channel(raw).await;
        let (probe, battery, _) = probe_features(
            &channel,
            1,
            None,
            Some(&ReceiverRegisterPhase::uncontended()),
        )
        .await;
        (probe, battery)
    }

    #[tokio::test]
    async fn hidpp10_device_is_probed_through_its_battery_register() {
        // 0x02: invalid address, the device's answer for a register it lacks.
        let (probe, battery) = hidpp10_probe(0x02).await;

        assert_eq!(probe.protocol, ProbedProtocol::Hidpp10);
        assert!(
            probe.completed(),
            "a HID++ 1.0 probe has no walk to wait for"
        );
        assert_eq!(
            probe.capabilities(),
            None,
            "no feature table, no measurement"
        );
        assert_eq!(battery, Some(BatteryProbe::StatusRegister));
    }

    #[tokio::test]
    async fn hidpp10_probe_fails_when_the_receiver_answers_for_the_device() {
        // 0x09: the receiver reports the device disconnected. That says
        // nothing about its registers, so nothing may be memoized.
        let (probe, battery) = hidpp10_probe(0x09).await;

        assert!(!probe.completed());
        assert_eq!(battery, None);
    }

    #[tokio::test]
    async fn hidpp10_device_outside_the_register_phase_is_not_probed() {
        // A wired HID++ 1.0 mouse, or one behind a receiver OpenLogi does not
        // recognize: no register phase is held, so its registers stay unread.
        // The responder answers them anyway, so a stray read fails the
        // assertion below instead of timing out.
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            match (request[2], request[3]) {
                (0x00, _) => Some(register_error(request, 0x01)),
                (0x81, register) => Some(vec![0x10, request[1], 0x81, register, 0x05, 0x00, 0x00]),
                _ => None,
            }
        });
        let channel = scripted_channel(raw).await;
        let (probe, battery, _) = probe_features(&channel, 0xff, None, None).await;
        assert!(!probe.completed());
        assert_eq!(battery, None);
        assert!(
            handle
                .written_reports()
                .iter()
                .all(|report| report.get(2) != Some(&0x81)),
            "no register read may go out without the register phase"
        );
    }

    #[tokio::test]
    async fn battery_registers_are_not_read_outside_the_register_phase() {
        let (raw, handle) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            Some(vec![0x10, request[1], 0x81, request[3], 0x05, 0x00, 0x00])
        });
        let channel = scripted_channel(raw).await;
        let writes_before = handle.written_reports().len();
        for probe in [BatteryProbe::StatusRegister, BatteryProbe::ChargeRegister] {
            assert_eq!(
                read_battery(&channel, 1, probe, None).await,
                BatteryRead::Unavailable
            );
        }
        assert_eq!(handle.written_reports().len(), writes_before);
    }

    #[tokio::test]
    async fn status_register_battery_reads_as_a_level_without_a_percentage() {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            assert_eq!((request[2], request[3]), (0x81, 0x07), "{request:02x?}");
            Some(vec![0x10, request[1], 0x81, 0x07, 0x03, 0x21, 0x00])
        });
        let channel = scripted_channel(raw).await;

        let BatteryRead::Reading(battery) = read_battery(
            &channel,
            1,
            BatteryProbe::StatusRegister,
            Some(&ReceiverRegisterPhase::uncontended()),
        )
        .await
        else {
            panic!("expected a reading");
        };
        assert_eq!(battery.percentage, None);
        assert_eq!(battery.level, BatteryLevel::Low);
        assert_eq!(battery.status, BatteryStatus::Charging);
    }

    #[tokio::test]
    async fn charge_register_battery_reads_its_percentage() {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(|request| {
            assert_eq!((request[2], request[3]), (0x81, 0x0d), "{request:02x?}");
            Some(vec![0x10, request[1], 0x81, 0x0d, 0x4b, 0x00, 0x30])
        });
        let channel = scripted_channel(raw).await;

        let BatteryRead::Reading(battery) = read_battery(
            &channel,
            1,
            BatteryProbe::ChargeRegister,
            Some(&ReceiverRegisterPhase::uncontended()),
        )
        .await
        else {
            panic!("expected a reading");
        };
        assert_eq!(battery.percentage, Some(75));
        assert_eq!(battery.level, BatteryLevel::Good);
        assert_eq!(battery.status, BatteryStatus::Discharging);
    }

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
        probe_features(&channel, 0xff, None, None).await.0
    }

    /// Probe a `DisableKeys` descriptor with the supplied feature-table flags.
    async fn disable_keys_probe(typ: FeatureType) -> ProbedFeatures {
        let features = [
            (0x0001_u16, FeatureType::empty()),
            (DisableKeysFeature::ID, typ),
        ];
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 1,
                (1, 0) => response[4] = u8::try_from(features.len()).unwrap(),
                (1, 1) => {
                    let (id, typ) = features[usize::from(request[4]) - 1];
                    response[4..6].copy_from_slice(&id.to_be_bytes());
                    response[6] = typ.bits();
                }
                _ => panic!("unexpected DisableKeys descriptor request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        probe_features(&channel, 0xff, None, None).await.0
    }

    #[tokio::test]
    async fn disable_keys_capability_ignores_hidden_and_engineering_descriptors() {
        for (typ, supported) in [
            (FeatureType::empty(), true),
            (FeatureType::HIDDEN, false),
            (FeatureType::ENGINEERING, false),
        ] {
            let capabilities = disable_keys_probe(typ)
                .await
                .capabilities()
                .expect("feature table responded");
            assert_eq!(capabilities.disable_keys, supported, "{typ:?}");
        }
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
                let caps = probe.capabilities().unwrap();
                assert!(!probe.capabilities_incomplete());
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
                let caps = probe.capabilities().unwrap();
                assert!(!probe.capabilities_incomplete());
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
            let caps = probe.capabilities().unwrap();
            assert_eq!(probe.capabilities_incomplete(), fail_at.is_some());
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
        probe_features(&channel, 0xff, None, None).await.0
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
            assert!(!probe.capabilities_incomplete());
            assert_eq!(
                probe.capabilities().unwrap().os_layout,
                settable,
                "capabilities {word:04x}"
            );
        }
        // An unanswered read leaves the set incomplete, so it is not memoized.
        let probe = multi_platform_probe(None).await;
        assert!(probe.capabilities_incomplete());
    }

    /// Probe a mouse whose `0x19c0` (at index 2) reports `count` buttons and
    /// answers `getButtonInfo` with `info`, or fails that read when it is
    /// `None`.
    async fn force_sensing_probe(count: u8, info: Option<[u8; 8]>) -> ProbedFeatures {
        let (raw, _) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            let features = [0x0001_u16, 0x19c0];
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                (0, 0) => response[4] = 1,
                (1, 0) => response[4] = 2,
                (1, 1) => {
                    let id = features[usize::from(request[4]) - 1];
                    response[4..6].copy_from_slice(&id.to_be_bytes());
                }
                (2, 0) => response[4] = count,
                (2, 1) => match info {
                    Some(info) => response[4..12].copy_from_slice(&info),
                    None => return Some(feature_error(request, 0x08)),
                },
                _ => panic!("unexpected capability request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        probe_features(&channel, 0xff, None, None).await.0
    }

    #[tokio::test]
    async fn force_sensing_needs_a_button_whose_force_software_can_change() {
        // The MX Master 4 Haptic Sense Panel: changeable, 5089 in 4071..=6768.
        const MX_MASTER_4: [u8; 8] = [0x00, 0x01, 0x13, 0xe1, 0x1a, 0x70, 0x0f, 0xe7];
        const FIXED: [u8; 8] = [0x00, 0x00, 0x13, 0xe1, 0x1a, 0x70, 0x0f, 0xe7];
        const INVERTED: [u8; 8] = [0x00, 0x01, 0x13, 0xe1, 0x0f, 0xe7, 0x1a, 0x70];
        const DEFAULT_OUTSIDE: [u8; 8] = [0x00, 0x01, 0x0f, 0x00, 0x1a, 0x70, 0x0f, 0xe7];
        for (count, info, offered) in [
            (1, MX_MASTER_4, true),
            (1, FIXED, false),
            (1, INVERTED, false),
            (1, DEFAULT_OUTSIDE, false),
            (0, MX_MASTER_4, false),
        ] {
            let probe = force_sensing_probe(count, Some(info)).await;
            assert!(!probe.capabilities_incomplete());
            assert_eq!(
                probe.capabilities().unwrap().force_sensing,
                offered,
                "count {count}, info {info:02x?}"
            );
        }
        // An unanswered read leaves the set incomplete, so it is not memoized.
        let probe = force_sensing_probe(1, None).await;
        assert!(probe.capabilities_incomplete());
    }

    /// How a `0x19c0` `getCount` read goes in [`force_sensing_count_probe`].
    #[derive(Clone, Copy, Debug)]
    enum CountRead {
        /// The device answers with this HID++ error code.
        FeatureError(u8),
        /// The request never reaches the device.
        TransportFailure,
    }

    /// Probe a mouse with `0x19c0` at index 2 and `0x1b04` at index 3 whose
    /// `getCount` goes as `count` describes. Its one control is DPI-family
    /// and divertable with raw XY, so a completed walk offers DPI gestures.
    async fn force_sensing_count_probe(count: CountRead) -> ProbedFeatures {
        fn is_get_count(request: &[u8]) -> bool {
            request[2] == 2 && request[3] >> 4 == 0
        }
        let responder = move |request: &[u8]| {
            let mut response = vec![0; 20];
            response[..4].copy_from_slice(&request[..4]);
            response[0] = 0x11;
            let features = [0x0001_u16, 0x19c0, 0x1b04];
            match (request[2], request[3] >> 4) {
                (0, 1) => response[4] = 4,
                // The feature set's index, and the 0x1b04 control count.
                (0 | 3, 0) => response[4] = 1,
                (1, 0) => response[4] = 3,
                (1, 1) => {
                    let id = features[usize::from(request[4]) - 1];
                    response[4..6].copy_from_slice(&id.to_be_bytes());
                }
                (2, 0) => match count {
                    CountRead::FeatureError(code) => return Some(feature_error(request, code)),
                    CountRead::TransportFailure => {
                        panic!("a failed write must not reach the device")
                    }
                },
                (3, 1) => {
                    response[4..6].copy_from_slice(&0x00ed_u16.to_be_bytes());
                    response[8] = 0x20;
                    response[12] = 0x01;
                }
                _ => panic!("unexpected capability request: {request:02x?}"),
            }
            Some(response)
        };
        let (raw, _) = match count {
            CountRead::TransportFailure => {
                ScriptedRawHidChannel::with_failing_writes(responder, is_get_count)
            }
            CountRead::FeatureError(_) => ScriptedRawHidChannel::with_dynamic_responder(responder),
        };
        let channel = scripted_channel(raw).await;
        probe_features(&channel, 0xff, None, None).await.0
    }

    #[tokio::test]
    async fn force_sensing_count_failure_is_incomplete_only_when_unanswered() {
        // Unknown (1), InvalidArgument (2), InvalidFunctionId (7) and
        // Unsupported (9) are answers; Busy (8) and a lost write are not.
        for (count, incomplete) in [
            (CountRead::FeatureError(0x01), false),
            (CountRead::FeatureError(0x02), false),
            (CountRead::FeatureError(0x07), false),
            (CountRead::FeatureError(0x09), false),
            (CountRead::FeatureError(0x08), true),
            (CountRead::TransportFailure, true),
        ] {
            let probe = force_sensing_count_probe(count).await;
            assert_eq!(probe.capabilities_incomplete(), incomplete, "{count:?}");
            let caps = probe.capabilities().unwrap();
            // An answer withdraws the button; an unanswered read keeps the
            // feature table's presumption for the cache to repair.
            assert_eq!(caps.force_sensing, incomplete, "{count:?}");
            // The control walk runs regardless of how 0x19c0 went.
            assert!(caps.dpi_gestures, "{count:?}");
        }
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
        let (probe, battery_probe, _) = probe_features(&channel, 0xff, None, None).await;
        let battery = match battery_probe {
            Some(battery_probe) => match read_battery(&channel, 0xff, battery_probe, None).await {
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
        assert!(!probe.capabilities_incomplete());
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
        assert!(!probe.capabilities_incomplete());
        assert_eq!(battery.expect("a reading").percentage, Some(67));
    }

    #[tokio::test]
    async fn a_failed_capability_read_keeps_the_percentage_and_reprobes() {
        let (probe, battery_probe, battery) = unified_probe(None, [67, 0x04, 0]).await;
        assert_eq!(battery_probe, Some(unified(2)));
        assert!(
            probe.capabilities_incomplete(),
            "an assumed capability must not be memoized"
        );
        assert_eq!(battery.expect("a reading").percentage, Some(67));
    }

    /// A `0x1000` source as the feature table alone describes it, before the
    /// probe reads its capability.
    const fn legacy(index: u8) -> BatteryProbe {
        BatteryProbe::Legacy {
            index,
            mileage: true,
        }
    }

    /// `0x1000` capability flags: bit 1 mileage, bit 2 rechargeable.
    const MILEAGE: u8 = 1 << 1;
    const LEGACY_RECHARGEABLE: u8 = 1 << 2;

    /// Probe a device whose only battery source is a `0x1000` at index 2, then
    /// read the battery through the probe the way the cache does.
    /// `capability` is the `getBatteryCapability` reply's level count and
    /// flags (`None` fails the read); `status` is the `getBatteryLevelStatus`
    /// reply.
    async fn legacy_probe(
        capability: Option<[u8; 2]>,
        status: [u8; 3],
    ) -> (ProbedFeatures, Option<BatteryProbe>, Option<BatteryInfo>) {
        let features = [0x0001, BatteryStatusFeature::ID];
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
                (2, 1) => {
                    let Some(capability) = capability else {
                        return Some(feature_error(request, 0x08));
                    };
                    response[4..6].copy_from_slice(&capability);
                }
                (2, 0) => response[4..7].copy_from_slice(&status),
                _ => panic!("unexpected request: {request:02x?}"),
            }
            Some(response)
        });
        let channel = scripted_channel(raw).await;
        let (probe, battery_probe, _) = probe_features(&channel, 0xff, None, None).await;
        let battery = match battery_probe {
            Some(battery_probe) => match read_battery(&channel, 0xff, battery_probe, None).await {
                BatteryRead::Reading(info) => Some(info),
                _ => None,
            },
            None => None,
        };
        (probe, battery_probe, battery)
    }

    #[tokio::test]
    async fn a_legacy_battery_without_mileage_reads_its_four_levels() {
        // Four levels, no mileage. 85 % is the draft's "full" range but the
        // display buckets' "good".
        let (probe, battery_probe, battery) =
            legacy_probe(Some([4, LEGACY_RECHARGEABLE]), [85, 50, 0]).await;
        assert_eq!(
            battery_probe,
            Some(BatteryProbe::Legacy {
                index: 2,
                mileage: false,
            })
        );
        assert!(!probe.capabilities_incomplete());
        let battery = battery.expect("a reading");
        assert_eq!(battery.percentage, Some(85));
        assert_eq!(battery.level, BatteryLevel::Full);
        assert_eq!(battery.status, BatteryStatus::Discharging);
    }

    #[tokio::test]
    async fn a_legacy_mileage_flag_below_ten_levels_is_ignored() {
        let (_, battery_probe, battery) = legacy_probe(Some([4, MILEAGE]), [15, 5, 0]).await;
        assert_eq!(
            battery_probe,
            Some(BatteryProbe::Legacy {
                index: 2,
                mileage: false,
            })
        );
        assert_eq!(battery.expect("a reading").level, BatteryLevel::Low);
    }

    #[tokio::test]
    async fn a_legacy_battery_with_mileage_keeps_the_display_buckets() {
        let (probe, battery_probe, battery) =
            legacy_probe(Some([100, MILEAGE | LEGACY_RECHARGEABLE]), [85, 84, 0]).await;
        assert_eq!(battery_probe, Some(legacy(2)));
        assert!(!probe.capabilities_incomplete());
        let battery = battery.expect("a reading");
        assert_eq!(battery.percentage, Some(85));
        assert_eq!(battery.level, BatteryLevel::Good);
    }

    #[tokio::test]
    async fn a_failed_legacy_capability_read_keeps_the_display_buckets_and_reprobes() {
        let (probe, battery_probe, battery) = legacy_probe(None, [85, 50, 0]).await;
        assert_eq!(battery_probe, Some(legacy(2)));
        assert!(
            probe.capabilities_incomplete(),
            "an assumed capability must not be memoized"
        );
        assert_eq!(battery.expect("a reading").level, BatteryLevel::Good);
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
        assert_eq!(battery_feature_index(table), Some(legacy(2)));
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
        assert_eq!(battery_feature_index(table), Some(legacy(2)));
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
        read_battery(&channel, 0xff, BatteryProbe::Adc(8), None).await
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
        let BatteryRead::Reading(battery) = read_battery(&channel, 0xff, legacy(2), None).await
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
            (BatteryStatusFeature::ID, legacy(2)),
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
