//! Lifecycle hints and latest backlight readings from inventory-owned HID++ channels.
//!
//! The persistent [`super::Enumerator`] opens each OS HID node once. This
//! module attaches one message listener to that existing channel and keeps
//! only the feature indexes needed to recognize unsolicited notifications.
//! Lifecycle hints request an authoritative inventory reconciliation; backlight
//! readings go through a separate latest-value channel to the transient OSD.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use hidpp::channel::{HidppChannel, HidppMessage, MessageListenerGuard};
use hidpp::feature::adc_measurement::AdcMeasurementFeature;
use hidpp::feature::backlight::BacklightEvent;
use hidpp::feature::unified_battery::BatteryEvent;
use hidpp::feature::wireless_device_status::WirelessDeviceStatusEvent;
use hidpp::protocol::{v10, v20};
use hidpp::receiver::{bolt, unifying};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::{mpsc, watch};

use crate::ReceiverProtocol;

/// A decoded HID++ notification, routed by the notifier to its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidppEventSource {
    /// A receiver reported a paired slot connecting or disconnecting.
    ReceiverConnection,
    /// A device's `WirelessDeviceStatus` feature reported reconnection.
    WirelessDeviceStatus,
    /// A device's event-capable `UnifiedBattery` feature reported a change.
    UnifiedBattery,
    /// A keyboard reported a changed backlight level.
    BacklightChanged(BacklightUpdate),
    /// A device's `AdcMeasurement` feature broadcast a reading or a link
    /// change — a headset dongle's headset switching on or off.
    AdcMeasurement,
}

/// Complete latest-value notification from a keyboard's backlight feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacklightUpdate {
    /// HID++ device index that emitted the event.
    pub device_index: u8,
    /// Current backlight level.
    pub current_level: u8,
    /// Number of selectable backlight levels, including zero.
    pub levels: u8,
    /// Whether the backlight is available for display.
    pub visible: bool,
}

/// The sending half of the HID++ notification channels.
///
/// Clones are installed only on inventory-owned channels. Lifecycle hints
/// share a capacity-one queue because each requests the same full scan;
/// backlight updates retain their latest value separately while a scan runs.
#[derive(Clone)]
pub struct EventNotifier {
    sender: mpsc::Sender<HidppEventSource>,
    backlight: watch::Sender<Option<BacklightUpdate>>,
    #[cfg(test)]
    observation: Option<Arc<EventObservation>>,
}

impl EventNotifier {
    fn notify(&self, source: HidppEventSource) {
        match source {
            HidppEventSource::BacklightChanged(update) => {
                self.backlight.send_replace(Some(update));
            }
            source => {
                let _ = self.sender.try_send(source);
            }
        }
        #[cfg(test)]
        if let Some(observation) = &self.observation {
            observation.count.fetch_add(1, Ordering::Release);
            observation.changed.notify_waiters();
        }
    }
}

/// The receiving half of the lifecycle queue and latest backlight value.
pub struct EventReceiver {
    lifecycle: mpsc::Receiver<HidppEventSource>,
    backlight: watch::Receiver<Option<BacklightUpdate>>,
}

impl EventReceiver {
    /// Receive the next inventory-reconciliation hint.
    pub async fn recv(&mut self) -> Option<HidppEventSource> {
        self.lifecycle.recv().await
    }

    /// Receive a queued hint without waiting.
    pub fn try_recv(&mut self) -> Result<HidppEventSource, mpsc::error::TryRecvError> {
        self.lifecycle.try_recv()
    }

    /// Subscribe to the latest backlight reading independently of inventory.
    #[must_use]
    pub fn backlights(&self) -> watch::Receiver<Option<BacklightUpdate>> {
        self.backlight.clone()
    }
}

/// Build the lifecycle and backlight channels used by an inventory watcher.
#[must_use]
pub fn event_channel() -> (EventNotifier, EventReceiver) {
    let (sender, receiver) = mpsc::channel(1);
    let (backlight, backlight_receiver) = watch::channel(None);
    (
        EventNotifier {
            sender,
            backlight,
            #[cfg(test)]
            observation: None,
        },
        EventReceiver {
            lifecycle: receiver,
            backlight: backlight_receiver,
        },
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
    let (backlight, backlight_receiver) = watch::channel(None);
    let observation = Arc::new(EventObservation {
        count: AtomicUsize::new(0),
        changed: Notify::new(),
    });
    (
        EventNotifier {
            sender,
            backlight,
            observation: Some(Arc::clone(&observation)),
        },
        EventReceiver {
            lifecycle: receiver,
            backlight: backlight_receiver,
        },
        EventObserver { observation },
    )
}

/// Runtime feature indexes whose unsolicited events affect inventory.
///
/// Stored with the immutable feature-table cache because these indexes are
/// discovered by the same walk. Unified battery and `0x1F20` ADC measurement
/// have battery events; legacy `0x1000` and voltage `0x1001` remain
/// recovery-scan reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct EventFeatureIndices {
    pub(super) wireless_status: Option<u8>,
    pub(super) unified_battery: Option<u8>,
    pub(super) backlight: Option<u8>,
    pub(super) adc_measurement: Option<u8>,
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
                0x1982 => indices.backlight = Some(index),
                0x1f20 => indices.adc_measurement = Some(index),
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
        if self.backlight == Some(header.feature_index)
            && let Some(BacklightEvent::InfoChanged(update)) =
                BacklightEvent::decode(function_id, &payload)
        {
            return Some(HidppEventSource::BacklightChanged(BacklightUpdate {
                device_index: header.device_index,
                current_level: update.current_level,
                levels: update.nb_levels,
                visible: !matches!(
                    update.status,
                    hidpp::feature::backlight::BacklightStatus::DisabledBySoftware
                        | hidpp::feature::backlight::BacklightStatus::DisabledByCriticalBattery
                        | hidpp::feature::backlight::BacklightStatus::AlsSaturated
                ),
            }));
        }
        // Unlike the two above, not gated on decoding the payload: an
        // unknown flags value still announces a link change.
        if self.adc_measurement == Some(header.feature_index)
            && AdcMeasurementFeature::is_status_broadcast(function_id)
        {
            return Some(HidppEventSource::AdcMeasurement);
        }
        None
    }
}

#[derive(Clone, Copy)]
struct DeviceEvents {
    device_index: u8,
    features: EventFeatureIndices,
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
        self.devices
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|device| device.device_index == device_index)
            .and_then(|device| device.features.recognizes(&message))
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
            });
        }
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
                backlight: None,
                adc_measurement: None,
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
            features: EventFeatureIndices {
                wireless_status: Some(5),
                unified_battery: None,
                backlight: None,
                adc_measurement: None,
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
            features: EventFeatureIndices {
                wireless_status: Some(5),
                unified_battery: Some(7),
                backlight: None,
                adc_measurement: None,
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
            features: EventFeatureIndices {
                wireless_status: None,
                unified_battery: None,
                backlight: None,
                adc_measurement: Some(8),
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

    #[tokio::test]
    async fn latest_backlight_survives_a_full_inventory_queue() {
        let (notifier, mut receiver) = event_channel();
        let mut backlights = receiver.backlights();
        notifier.notify(HidppEventSource::ReceiverConnection);
        notifier.notify(HidppEventSource::BacklightChanged(BacklightUpdate {
            device_index: 2,
            current_level: 3,
            levels: 8,
            visible: true,
        }));
        notifier.notify(HidppEventSource::BacklightChanged(BacklightUpdate {
            device_index: 2,
            current_level: 7,
            levels: 8,
            visible: true,
        }));

        backlights.changed().await.expect("sender is live");
        assert_eq!(
            *backlights.borrow_and_update(),
            Some(BacklightUpdate {
                device_index: 2,
                current_level: 7,
                levels: 8,
                visible: true,
            })
        );
        assert_eq!(
            receiver.try_recv(),
            Ok(HidppEventSource::ReceiverConnection)
        );
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }
}
