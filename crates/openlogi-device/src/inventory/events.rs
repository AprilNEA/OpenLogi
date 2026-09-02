//! Coalesced lifecycle events from inventory-owned HID++ channels.
//!
//! The persistent [`super::Enumerator`] opens each OS HID node once. This
//! module attaches one message listener to that existing channel and keeps
//! only the feature indexes needed to recognize lifecycle notifications. The
//! events never mutate inventory: they request another authoritative
//! reconciliation from the enumerator.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use hidpp::channel::{HidppChannel, HidppMessage, MessageListenerGuard};
use hidpp::feature::unified_battery::BatteryEvent;
use hidpp::feature::wireless_device_status::WirelessDeviceStatusEvent;
use hidpp::protocol::{v10, v20};
use hidpp::receiver::{bolt, unifying};
use serde::{Deserialize, Serialize};
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
}

/// The sending half of the bounded HID++ reconciliation-request channel.
///
/// Clones are installed only on inventory-owned channels. Capacity is one:
/// every source asks for the same full reconciliation, so a burst has no
/// additional meaning and must not grow memory while a slow probe is running.
#[derive(Clone)]
pub struct EventNotifier {
    sender: mpsc::Sender<HidppEventSource>,
}

impl EventNotifier {
    fn notify(&self, source: HidppEventSource) {
        let _ = self.sender.try_send(source);
    }
}

/// The receiving half of the coalesced HID++ reconciliation-request channel.
pub type EventReceiver = mpsc::Receiver<HidppEventSource>;

/// Build the bounded channel used by an inventory watcher and its enumerator.
#[must_use]
pub fn event_channel() -> (EventNotifier, EventReceiver) {
    let (sender, receiver) = mpsc::channel(1);
    (EventNotifier { sender }, receiver)
}

/// Runtime feature indexes whose unsolicited events affect inventory.
///
/// Stored with the immutable feature-table cache because these indexes are
/// discovered by the same walk. Only unified battery has a battery event;
/// legacy `0x1000` and voltage `0x1001` remain recovery-scan reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct EventFeatureIndices {
    pub(super) wireless_status: Option<u8>,
    pub(super) unified_battery: Option<u8>,
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
