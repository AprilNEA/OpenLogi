//! Coalesced lifecycle events from inventory-owned HID++ channels.
//!
//! The persistent [`super::Enumerator`] opens each OS HID node once. This
//! module attaches one message listener to that existing channel and keeps
//! only the feature indexes needed to recognize lifecycle notifications. The
//! events never mutate inventory: they request another authoritative
//! reconciliation from the enumerator.
//!
//! A broadcast that announces a change to a device *setting* — Fn lock
//! toggled on the keyboard, the DPI changed by a mouse button — also bumps
//! that device's settings revision here, which the next reconciliation
//! publishes as [`PairedDevice::settings_revision`]. A client re-reads the
//! device's settings when it sees the revision move.
//!
//! [`PairedDevice::settings_revision`]: openlogi_core::device::PairedDevice::settings_revision

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use hidpp::channel::{HidppChannel, HidppMessage, MessageListenerGuard};
use hidpp::feature::adc_measurement::AdcMeasurementFeature;
use hidpp::feature::battery_status::BatteryStatusFeature;
use hidpp::feature::battery_voltage::BatteryVoltageFeature;
use hidpp::feature::extended_dpi::ExtendedDpiFeature;
use hidpp::feature::fn_inversion::{
    FnInversionMultiHostFeature, FnInversionWithDefaultStateFeature,
};
use hidpp::feature::multi_platform::PlatformChange;
use hidpp::feature::unified_battery::BatteryEvent;
use hidpp::feature::wireless_device_status::WirelessDeviceStatusEvent;
use hidpp::protocol::{v10, v20};
use hidpp::receiver::{bolt, unifying};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::mpsc;

use crate::ReceiverProtocol;

/// A HID++ lifecycle source that requested authoritative inventory
/// reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidppEventSource {
    /// A receiver reported a paired slot connecting or disconnecting.
    ReceiverConnection,
    /// A device's `WirelessDeviceStatus` feature reported reconnection.
    WirelessDeviceStatus,
    /// A device's event-capable `UnifiedBattery` feature reported a change.
    UnifiedBattery,
    /// A device's legacy `BatteryStatus` (`0x1000`) feature broadcast a new
    /// level or charging status.
    BatteryStatus,
    /// A device's `BatteryVoltage` (`0x1001`) feature broadcast a new voltage
    /// or charging state.
    BatteryVoltage,
    /// A device's `AdcMeasurement` feature broadcast a reading or a link
    /// change — a headset dongle's headset switching on or off.
    AdcMeasurement,
    /// A keyboard's Fn-inversion feature (`0x40a2` or `0x40a3`) reported its
    /// Fn lock changing on the device, as Fn+Esc does.
    FnInversion,
    /// A mouse's `ExtendedAdjustableDpi` (`0x2202`) feature reported its
    /// sensor DPI changing on the device, as a DPI button does.
    ExtendedDpi,
    /// A keyboard's `MultiPlatform` (`0x4531`) feature reported its key
    /// layout platform changing on the device, as a platform key combination
    /// does.
    MultiPlatform,
}

impl HidppEventSource {
    /// Whether this source announces a change to a device setting a client
    /// shows, rather than to the device's presence or battery.
    const fn changes_settings(self) -> bool {
        matches!(
            self,
            Self::FnInversion | Self::ExtendedDpi | Self::MultiPlatform
        )
    }
}

/// The sending half of the bounded HID++ reconciliation-request channel.
///
/// Clones are installed only on inventory-owned channels. Capacity is one:
/// every source asks for the same full reconciliation, so a burst has no
/// additional meaning and must not grow memory while a slow probe is running.
#[derive(Clone)]
pub struct EventNotifier {
    sender: mpsc::Sender<HidppEventSource>,
    #[cfg(test)]
    observation: Option<Arc<EventObservation>>,
}

impl EventNotifier {
    fn notify(&self, source: HidppEventSource) {
        let _ = self.sender.try_send(source);
        #[cfg(test)]
        if let Some(observation) = &self.observation {
            observation.count.fetch_add(1, Ordering::Release);
            observation.changed.notify_waiters();
        }
    }
}

/// The receiving half of the coalesced HID++ reconciliation-request channel.
pub type EventReceiver = mpsc::Receiver<HidppEventSource>;

/// Build the bounded channel used by an inventory watcher and its enumerator.
#[must_use]
pub fn event_channel() -> (EventNotifier, EventReceiver) {
    let (sender, receiver) = mpsc::channel(1);
    (
        EventNotifier {
            sender,
            #[cfg(test)]
            observation: None,
        },
        receiver,
    )
}

#[cfg(test)]
struct EventObservation {
    count: AtomicUsize,
    changed: Notify,
}

/// Barrier observing decoded lifecycle events, including coalesced requests.
#[cfg(test)]
pub(crate) struct EventObserver {
    observation: Arc<EventObservation>,
}

#[cfg(test)]
impl EventObserver {
    pub(crate) async fn wait_for(&self, expected: usize) {
        loop {
            let changed = self.observation.changed.notified();
            if self.observation.count.load(Ordering::Acquire) >= expected {
                return;
            }
            changed.await;
        }
    }
}

/// Build the production capacity-one event channel plus a decode observation barrier.
#[cfg(test)]
pub(crate) fn observed_event_channel() -> (EventNotifier, EventReceiver, EventObserver) {
    let (sender, receiver) = mpsc::channel(1);
    let observation = Arc::new(EventObservation {
        count: AtomicUsize::new(0),
        changed: Notify::new(),
    });
    (
        EventNotifier {
            sender,
            observation: Some(Arc::clone(&observation)),
        },
        receiver,
        EventObserver { observation },
    )
}

/// Runtime feature indexes whose unsolicited events affect inventory.
///
/// Stored with the immutable feature-table cache because these indexes are
/// discovered by the same walk. Every battery source the probe reads —
/// unified `0x1004`, legacy `0x1000`, voltage `0x1001` and `0x1F20` ADC
/// measurement — broadcasts its changes, so each has an index here. So do the
/// settings a user can change on the device itself: Fn lock (`0x40a2`,
/// `0x40a3`), extended DPI (`0x2202`) and the keyboard platform (`0x4531`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct EventFeatureIndices {
    pub(super) wireless_status: Option<u8>,
    pub(super) unified_battery: Option<u8>,
    pub(super) battery_status: Option<u8>,
    pub(super) battery_voltage: Option<u8>,
    pub(super) adc_measurement: Option<u8>,
    pub(super) fn_inversion: Option<u8>,
    pub(super) fn_inversion_multi_host: Option<u8>,
    pub(super) extended_dpi: Option<u8>,
    pub(super) multi_platform: Option<u8>,
}

impl EventFeatureIndices {
    pub(super) fn from_feature_ids(ids: &[u16]) -> Self {
        let mut indices = Self::default();
        for (position, id) in ids.iter().copied().enumerate() {
            let Ok(index) = u8::try_from(position + 1) else {
                break;
            };
            match id {
                0x1d4b => indices.wireless_status = Some(index),
                0x1004 => indices.unified_battery = Some(index),
                0x1000 => indices.battery_status = Some(index),
                0x1001 => indices.battery_voltage = Some(index),
                0x1f20 => indices.adc_measurement = Some(index),
                0x40a2 => indices.fn_inversion = Some(index),
                0x40a3 => indices.fn_inversion_multi_host = Some(index),
                0x2202 => indices.extended_dpi = Some(index),
                0x4531 => indices.multi_platform = Some(index),
                _ => {}
            }
        }
        indices
    }

    fn recognizes(self, message: &v20::Message) -> Option<HidppEventSource> {
        let header = message.header();
        if header.software_id.to_lo() != 0 {
            return None;
        }
        let function_id = header.function_id.to_lo();
        let payload = message.extend_payload();
        if self.wireless_status == Some(header.feature_index)
            && WirelessDeviceStatusEvent::decode(function_id, &payload).is_some()
        {
            return Some(HidppEventSource::WirelessDeviceStatus);
        }
        if self.unified_battery == Some(header.feature_index)
            && BatteryEvent::decode(function_id, &payload).is_some()
        {
            return Some(HidppEventSource::UnifiedBattery);
        }
        // Unlike the two above, the rest are not gated on decoding the
        // payload: the reconciliation re-reads the battery, and an unknown
        // status or flags value still announces a change.
        if self.battery_status == Some(header.feature_index)
            && BatteryStatusFeature::is_status_broadcast(function_id)
        {
            return Some(HidppEventSource::BatteryStatus);
        }
        if self.battery_voltage == Some(header.feature_index)
            && BatteryVoltageFeature::is_status_broadcast(function_id)
        {
            return Some(HidppEventSource::BatteryVoltage);
        }
        if self.adc_measurement == Some(header.feature_index)
            && AdcMeasurementFeature::is_status_broadcast(function_id)
        {
            return Some(HidppEventSource::AdcMeasurement);
        }
        // Neither are the settings: a client re-reads the setting itself.
        if self.fn_inversion == Some(header.feature_index)
            && FnInversionWithDefaultStateFeature::is_state_broadcast(function_id)
        {
            return Some(HidppEventSource::FnInversion);
        }
        if self.fn_inversion_multi_host == Some(header.feature_index)
            && FnInversionMultiHostFeature::is_state_broadcast(message)
        {
            return Some(HidppEventSource::FnInversion);
        }
        if self.extended_dpi == Some(header.feature_index)
            && ExtendedDpiFeature::is_parameters_broadcast(function_id)
        {
            return Some(HidppEventSource::ExtendedDpi);
        }
        // Logi Options+ takes every unsolicited `0x4531` report for a platform
        // change, whatever its function.
        if self.multi_platform == Some(header.feature_index)
            && PlatformChange::decode(&payload).is_some()
        {
            return Some(HidppEventSource::MultiPlatform);
        }
        None
    }
}

#[derive(Clone, Copy)]
struct DeviceEvents {
    device_index: u8,
    features: EventFeatureIndices,
    /// How many setting-change broadcasts this device index has sent on the
    /// channel. Owned here, never reset by [`EventSubscriptionHandle::register_device`]:
    /// a re-probe does not undo a change the user made on the device.
    settings_revision: u64,
}

struct SubscriptionState {
    protocol: Option<ReceiverProtocol>,
    devices: RwLock<Vec<DeviceEvents>>,
    receiver_snapshot_depth: AtomicUsize,
    notifier: EventNotifier,
}

impl SubscriptionState {
    fn decode(&self, raw: HidppMessage, matched: bool) -> Option<HidppEventSource> {
        if matched {
            return None;
        }

        let receiver_connection = match self.protocol {
            Some(ReceiverProtocol::Bolt) => matches!(
                bolt::decode_notification(&v10::Message::from(raw)),
                Some(bolt::Event::DeviceConnection(_))
            ),
            Some(ReceiverProtocol::Unifying) => matches!(
                unifying::decode_notification(&v10::Message::from(raw)),
                Some(unifying::Event::DeviceConnection(_))
            ),
            None => false,
        };
        if receiver_connection {
            return (self.receiver_snapshot_depth.load(Ordering::Acquire) == 0)
                .then_some(HidppEventSource::ReceiverConnection);
        }

        let message = v20::Message::from(raw);
        let device_index = message.header().device_index;
        let source = self
            .devices
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|device| device.device_index == device_index)
            .and_then(|device| device.features.recognizes(&message))?;
        if source.changes_settings() {
            // Bumped before the reconciliation request is sent, so the
            // snapshot that request produces already carries it.
            if let Some(device) = self
                .devices
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .iter_mut()
                .find(|device| device.device_index == device_index)
            {
                device.settings_revision = device.settings_revision.wrapping_add(1);
            }
        }
        Some(source)
    }
}

/// Cloneable registration handle passed through a node's probe.
#[derive(Clone)]
pub(super) struct EventSubscriptionHandle {
    state: Arc<SubscriptionState>,
}

impl EventSubscriptionHandle {
    /// Replace the decoder metadata for `device_index` as soon as a valid
    /// feature table (or its cached equivalent) is available. The channel
    /// listener was installed before the probe began, closing the wakeup race
    /// before the resulting snapshot is published.
    pub(super) fn register_device(&self, device_index: u8, features: EventFeatureIndices) {
        let mut devices = self
            .state
            .devices
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(device) = devices
            .iter_mut()
            .find(|device| device.device_index == device_index)
        {
            device.features = features;
        } else {
            devices.push(DeviceEvents {
                device_index,
                features,
                settings_revision: 0,
            });
        }
    }

    /// How many setting-change broadcasts `device_index` has sent on this
    /// channel; `0` before its first, or when it was never registered.
    pub(super) fn settings_revision(&self, device_index: u8) -> u64 {
        self.state
            .devices
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|device| device.device_index == device_index)
            .map_or(0, |device| device.settings_revision)
    }

    /// Suppress receiver connection events fabricated by the enumerator's own
    /// arrival trigger. A real event in this interval is covered by the probe
    /// already in progress; events immediately before or after remain queued.
    pub(super) fn begin_receiver_snapshot(&self) -> ReceiverSnapshotGuard {
        self.state
            .receiver_snapshot_depth
            .fetch_add(1, Ordering::AcqRel);
        ReceiverSnapshotGuard {
            state: Arc::clone(&self.state),
        }
    }
}

/// Persistent listener owned alongside one cached HID++ channel.
pub(super) struct ChannelEventSubscriptions {
    handle: EventSubscriptionHandle,
    _listener: MessageListenerGuard,
}

impl ChannelEventSubscriptions {
    pub(super) fn attach(
        channel: &Arc<HidppChannel>,
        protocol: Option<ReceiverProtocol>,
        notifier: EventNotifier,
    ) -> Self {
        let state = Arc::new(SubscriptionState {
            protocol,
            devices: RwLock::new(Vec::new()),
            receiver_snapshot_depth: AtomicUsize::new(0),
            notifier,
        });
        let listener = channel.add_msg_listener_guarded({
            let state = Arc::clone(&state);
            move |raw, matched| {
                if let Some(source) = state.decode(raw, matched) {
                    state.notifier.notify(source);
                }
            }
        });
        Self {
            handle: EventSubscriptionHandle { state },
            _listener: listener,
        }
    }

    pub(super) fn handle(&self) -> EventSubscriptionHandle {
        self.handle.clone()
    }
}

/// Receiver-arrival suppression scoped to one authoritative snapshot.
pub(super) struct ReceiverSnapshotGuard {
    state: Arc<SubscriptionState>,
}

impl Drop for ReceiverSnapshotGuard {
    fn drop(&mut self) {
        self.state
            .receiver_snapshot_depth
            .fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use hidpp::nibble::U4;
    use hidpp::protocol::{v10, v20};

    use super::*;

    fn state(protocol: Option<ReceiverProtocol>) -> Arc<SubscriptionState> {
        let (notifier, _receiver) = event_channel();
        Arc::new(SubscriptionState {
            protocol,
            devices: RwLock::new(Vec::new()),
            receiver_snapshot_depth: AtomicUsize::new(0),
            notifier,
        })
    }

    #[test]
    fn feature_indices_are_runtime_table_positions() {
        assert_eq!(
            EventFeatureIndices::from_feature_ids(&[0x0001, 0x1d4b, 0x1004]),
            EventFeatureIndices {
                wireless_status: Some(2),
                unified_battery: Some(3),
                battery_status: None,
                battery_voltage: None,
                adc_measurement: None,
                fn_inversion: None,
                fn_inversion_multi_host: None,
                extended_dpi: None,
                multi_platform: None,
            }
        );
    }

    #[test]
    fn receiver_snapshot_suppresses_only_triggered_connection_wakeups() {
        let state = state(Some(ReceiverProtocol::Unifying));
        let raw = v10::Message::Short(
            v10::MessageHeader {
                device_index: 2,
                sub_id: 0x41,
            },
            [0, 0x02, 0x34, 0x12],
        )
        .into();

        assert_eq!(
            state.decode(raw, false),
            Some(HidppEventSource::ReceiverConnection)
        );
        state.receiver_snapshot_depth.store(1, Ordering::Release);
        assert_eq!(state.decode(raw, false), None);
    }

    #[test]
    fn receiver_snapshot_does_not_suppress_device_feature_events() {
        let state = state(Some(ReceiverProtocol::Bolt));
        state.devices.write().unwrap().push(DeviceEvents {
            device_index: 2,
            settings_revision: 0,
            features: EventFeatureIndices {
                wireless_status: Some(5),
                unified_battery: None,
                battery_status: None,
                battery_voltage: None,
                adc_measurement: None,
                fn_inversion: None,
                fn_inversion_multi_host: None,
                extended_dpi: None,
                multi_platform: None,
            },
        });
        state.receiver_snapshot_depth.store(1, Ordering::Release);
        let wireless = v20::Message::Long(
            v20::MessageHeader {
                device_index: 2,
                feature_index: 5,
                function_id: U4::from_lo(0),
                software_id: U4::from_lo(0),
            },
            [0; 16],
        )
        .into();

        assert_eq!(
            state.decode(wireless, false),
            Some(HidppEventSource::WirelessDeviceStatus)
        );
    }

    #[test]
    fn registered_device_events_decode_to_typed_sources() {
        let state = state(None);
        state.devices.write().unwrap().push(DeviceEvents {
            device_index: 3,
            settings_revision: 0,
            features: EventFeatureIndices {
                wireless_status: Some(5),
                unified_battery: Some(7),
                battery_status: None,
                battery_voltage: None,
                adc_measurement: None,
                fn_inversion: None,
                fn_inversion_multi_host: None,
                extended_dpi: None,
                multi_platform: None,
            },
        });

        let wireless = v20::Message::Long(
            v20::MessageHeader {
                device_index: 3,
                feature_index: 5,
                function_id: U4::from_lo(0),
                software_id: U4::from_lo(0),
            },
            [0; 16],
        )
        .into();
        assert_eq!(
            state.decode(wireless, false),
            Some(HidppEventSource::WirelessDeviceStatus)
        );

        let mut payload = [0; 16];
        payload[0] = 80;
        payload[1] = 4;
        let battery = v20::Message::Long(
            v20::MessageHeader {
                device_index: 3,
                feature_index: 7,
                function_id: U4::from_lo(0),
                software_id: U4::from_lo(0),
            },
            payload,
        )
        .into();
        assert_eq!(
            state.decode(battery, false),
            Some(HidppEventSource::UnifiedBattery)
        );
    }

    #[test]
    fn adc_measurement_index_is_its_table_position() {
        // The G733 feature table, 0x1F20 at runtime index 8.
        let table = [
            0x0001, 0x0003, 0x0005, 0x8070, 0x8010, 0x8310, 0x8300, 0x1f20,
        ];
        assert_eq!(
            EventFeatureIndices::from_feature_ids(&table).adc_measurement,
            Some(8)
        );
    }

    #[test]
    fn adc_measurement_broadcasts_request_reconciliation() {
        let state = state(None);
        state.devices.write().unwrap().push(DeviceEvents {
            device_index: 0xff,
            settings_revision: 0,
            features: EventFeatureIndices {
                wireless_status: None,
                unified_battery: None,
                battery_status: None,
                battery_voltage: None,
                adc_measurement: Some(8),
                fn_inversion: None,
                fn_inversion_multi_host: None,
                extended_dpi: None,
                multi_platform: None,
            },
        });
        let event = |function_id: u8, software_id: u8, bytes: [u8; 3]| {
            let mut payload = [0; 16];
            payload[..3].copy_from_slice(&bytes);
            v20::Message::Long(
                v20::MessageHeader {
                    device_index: 0xff,
                    feature_index: 8,
                    function_id: U4::from_lo(function_id),
                    software_id: U4::from_lo(software_id),
                },
                payload,
            )
            .into()
        };

        // Both broadcasts captured from a G733 dongle: `11 ff 08 00 00 00 00`
        // as the headset switched off, `11 ff 08 00 10 4a 03` as it switched
        // back on while charging.
        for bytes in [[0x00, 0x00, 0x00], [0x10, 0x4a, 0x03]] {
            assert_eq!(
                state.decode(event(0, 0, bytes), false),
                Some(HidppEventSource::AdcMeasurement),
                "{bytes:02x?}"
            );
        }
        // A broadcast whose flags value is unknown (the kernel's `0x0F`) still
        // announces a change; the reconciliation's read decides what it means.
        assert_eq!(
            state.decode(event(0, 0, [0x10, 0x4a, 0x0f]), false),
            Some(HidppEventSource::AdcMeasurement)
        );
        // A reply to our own request carries a software id; not an event.
        assert_eq!(state.decode(event(0, 0xd, [0x10, 0x4a, 0x01]), false), None);
        // Only event `0` is the status broadcast.
        assert_eq!(state.decode(event(1, 0, [0x10, 0x4a, 0x01]), false), None);
    }

    #[test]
    fn legacy_and_voltage_battery_indexes_are_table_positions() {
        // An MX Master 2S-shaped table: 0x1000 instead of 0x1004.
        let legacy =
            EventFeatureIndices::from_feature_ids(&[0x0001, 0x0003, 0x0005, 0x1000, 0x1d4b]);
        assert_eq!(legacy.battery_status, Some(4));
        assert_eq!(legacy.battery_voltage, None);
        assert_eq!(legacy.unified_battery, None);
        // A G-series-shaped table: 0x1001 and no other battery source.
        let voltage = EventFeatureIndices::from_feature_ids(&[0x0001, 0x0003, 0x1001, 0x8070]);
        assert_eq!(voltage.battery_voltage, Some(3));
        assert_eq!(voltage.battery_status, None);
    }

    #[test]
    fn legacy_and_voltage_battery_broadcasts_request_reconciliation() {
        let state = state(None);
        state.devices.write().unwrap().push(DeviceEvents {
            device_index: 1,
            settings_revision: 0,
            features: EventFeatureIndices {
                battery_status: Some(4),
                battery_voltage: Some(6),
                ..EventFeatureIndices::default()
            },
        });
        let event = |feature_index: u8, function_id: u8, software_id: u8, bytes: [u8; 3]| {
            let mut payload = [0; 16];
            payload[..3].copy_from_slice(&bytes);
            v20::Message::Long(
                v20::MessageHeader {
                    device_index: 1,
                    feature_index,
                    function_id: U4::from_lo(function_id),
                    software_id: U4::from_lo(software_id),
                },
                payload,
            )
            .into()
        };

        // 0x1000: level 50 %, next 20 %, discharging; then a status value
        // (`0x09`) the crate does not know, which still announces a change.
        for bytes in [[50, 20, 0x00], [0, 0, 0x09]] {
            assert_eq!(
                state.decode(event(4, 0, 0, bytes), false),
                Some(HidppEventSource::BatteryStatus),
                "{bytes:02x?}"
            );
        }
        // 0x1001: 3.9 V while charging (flags bit 7).
        assert_eq!(
            state.decode(event(6, 0, 0, [0x0f, 0x3c, 0x80]), false),
            Some(HidppEventSource::BatteryVoltage)
        );
        for (feature_index, source) in [(4, "0x1000"), (6, "0x1001")] {
            // The reply to our own read is matched to its request, and a reply
            // carries a software id: neither is a broadcast.
            assert_eq!(
                state.decode(event(feature_index, 0, 0, [50, 20, 0]), true),
                None,
                "{source}"
            );
            assert_eq!(
                state.decode(event(feature_index, 0, 0xd, [50, 20, 0]), false),
                None,
                "{source}"
            );
            // Only event `0` is the status broadcast.
            assert_eq!(
                state.decode(event(feature_index, 1, 0, [50, 20, 0]), false),
                None,
                "{source}"
            );
        }
        // Another feature's index on the same device is not a battery event.
        assert_eq!(state.decode(event(5, 0, 0, [50, 20, 0]), false), None);
    }

    #[test]
    fn setting_feature_indexes_are_table_positions() {
        // An MX Keys-shaped table with 0x40a3, and a G502-shaped one with
        // 0x2202; 0x40a2 is the single-host keyboards' predecessor.
        let keyboard = EventFeatureIndices::from_feature_ids(&[0x0001, 0x0003, 0x1d4b, 0x40a3]);
        assert_eq!(keyboard.fn_inversion_multi_host, Some(4));
        assert_eq!(keyboard.fn_inversion, None);
        let single_host = EventFeatureIndices::from_feature_ids(&[0x0001, 0x40a2]);
        assert_eq!(single_host.fn_inversion, Some(2));
        let mouse = EventFeatureIndices::from_feature_ids(&[0x0001, 0x0003, 0x2202, 0x1001]);
        assert_eq!(mouse.extended_dpi, Some(3));
        assert_eq!(mouse.battery_voltage, Some(4));
    }

    #[test]
    fn setting_broadcasts_request_reconciliation_and_bump_the_revision() {
        let state = state(Some(ReceiverProtocol::Bolt));
        let handle = EventSubscriptionHandle {
            state: Arc::clone(&state),
        };
        let features = EventFeatureIndices {
            fn_inversion: Some(4),
            fn_inversion_multi_host: Some(5),
            extended_dpi: Some(6),
            battery_status: Some(7),
            ..EventFeatureIndices::default()
        };
        handle.register_device(1, features);
        handle.register_device(2, features);
        let long = |device_index: u8, feature_index: u8, function_id: u8, software_id: u8| {
            v20::Message::Long(
                v20::MessageHeader {
                    device_index,
                    feature_index,
                    function_id: U4::from_lo(function_id),
                    software_id: U4::from_lo(software_id),
                },
                [0; 16],
            )
            .into()
        };
        assert_eq!(handle.settings_revision(1), 0);

        // 0x40a2: function 1 only.
        assert_eq!(
            state.decode(long(1, 4, 1, 0), false),
            Some(HidppEventSource::FnInversion)
        );
        assert_eq!(handle.settings_revision(1), 1);
        assert_eq!(state.decode(long(1, 4, 0, 0), false), None);
        // 0x40a3: any function, but only a long report.
        assert_eq!(
            state.decode(long(1, 5, 0, 0), false),
            Some(HidppEventSource::FnInversion)
        );
        let short = v20::Message::Short(
            v20::MessageHeader {
                device_index: 1,
                feature_index: 5,
                function_id: U4::from_lo(0),
                software_id: U4::from_lo(0),
            },
            [1, 1, 0],
        )
        .into();
        assert_eq!(state.decode(short, false), None);
        assert_eq!(handle.settings_revision(1), 2);
        // 0x2202: the parameters broadcast, not a calibration result.
        assert_eq!(
            state.decode(long(1, 6, 0, 0), false),
            Some(HidppEventSource::ExtendedDpi)
        );
        assert_eq!(state.decode(long(1, 6, 1, 0), false), None);
        assert_eq!(handle.settings_revision(1), 3);

        // Our own traffic is not a device-side change: a matched reply, or a
        // message carrying a software id.
        assert_eq!(state.decode(long(1, 5, 1, 0), true), None);
        assert_eq!(state.decode(long(1, 5, 1, 0xd), false), None);
        // A battery broadcast requests reconciliation but changes no setting.
        assert_eq!(
            state.decode(long(1, 7, 0, 0), false),
            Some(HidppEventSource::BatteryStatus)
        );
        assert_eq!(handle.settings_revision(1), 3);

        // A re-probe re-registers the device; the change it already made
        // stands.
        handle.register_device(1, features);
        assert_eq!(handle.settings_revision(1), 3);
        // The revision is per device index, and an unknown index has none.
        assert_eq!(handle.settings_revision(2), 0);
        assert_eq!(state.decode(long(3, 5, 0, 0), false), None);
        assert_eq!(handle.settings_revision(3), 0);
    }

    #[test]
    fn platform_change_reports_bump_the_revision() {
        let keyboard = EventFeatureIndices::from_feature_ids(&[0x0001, 0x0003, 0x40a3, 0x4531]);
        assert_eq!(keyboard.multi_platform, Some(4));

        let state = state(Some(ReceiverProtocol::Bolt));
        let handle = EventSubscriptionHandle {
            state: Arc::clone(&state),
        };
        handle.register_device(1, keyboard);
        let report = |function_id: u8, software_id: u8| {
            v20::Message::Short(
                v20::MessageHeader {
                    device_index: 1,
                    feature_index: 4,
                    function_id: U4::from_lo(function_id),
                    software_id: U4::from_lo(software_id),
                },
                [1, 2, 2],
            )
            .into()
        };

        // Options+ takes any unsolicited 0x4531 report for a platform change.
        for (function_id, revision) in [(0, 1), (1, 2)] {
            assert_eq!(
                state.decode(report(function_id, 0), false),
                Some(HidppEventSource::MultiPlatform)
            );
            assert_eq!(handle.settings_revision(1), revision);
        }
        // A reply to our own request is not.
        assert_eq!(state.decode(report(2, 0), true), None);
        assert_eq!(state.decode(report(2, 0xd), false), None);
        assert_eq!(handle.settings_revision(1), 2);
    }

    #[test]
    fn event_requests_are_bounded_and_coalesced() {
        let (notifier, mut receiver) = event_channel();
        notifier.notify(HidppEventSource::ReceiverConnection);
        notifier.notify(HidppEventSource::UnifiedBattery);

        assert_eq!(
            receiver.try_recv(),
            Ok(HidppEventSource::ReceiverConnection)
        );
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }
}
