//! Implements the `WirelessDeviceStatus` feature (ID `0x1d4b`) that notifies
//! the host about device reconnections.

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
    channel::HidppChannel,
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, event_payload},
};

/// Implements the `WirelessDeviceStatus` / `0x1d4b` feature.
pub struct WirelessDeviceStatusFeature {
    /// The underlying HID++ channel.
    chan: Arc<HidppChannel>,

    /// The emitter used to emit events.
    emitter: Arc<EventEmitter<WirelessDeviceStatusEvent>>,

    /// The handle assigned to the message listener registered via
    /// [`HidppChannel::add_msg_listener`].
    /// This is used to remove the listener when the feature is dropped.
    msg_listener_hdl: u32,
}

impl CreatableFeature for WirelessDeviceStatusFeature {
    const ID: u16 = 0x1d4b;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let hdl = chan.add_msg_listener({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                // The reconnection broadcast is the only event and carries sub-id 0.
                if func.to_lo() != 0 {
                    return;
                }

                let (Ok(status), Ok(request), Ok(reason)) = (
                    WirelessDeviceStatus::try_from(payload[0]),
                    WirelessDeviceStatusRequest::try_from(payload[1]),
                    WirelessDeviceStatusReason::try_from(payload[2]),
                ) else {
                    return;
                };

                emitter.emit(WirelessDeviceStatusEvent::StatusBroadcast(
                    WirelessDeviceStatusBroadcast {
                        status,
                        request,
                        reason,
                    },
                ));
            }
        });

        Self {
            chan,
            emitter,
            msg_listener_hdl: hdl,
        }
    }
}

impl Feature for WirelessDeviceStatusFeature {}

impl EmittingFeature<WirelessDeviceStatusEvent> for WirelessDeviceStatusFeature {
    fn listen(&self) -> async_channel::Receiver<WirelessDeviceStatusEvent> {
        self.emitter.create_receiver()
    }
}

impl Drop for WirelessDeviceStatusFeature {
    fn drop(&mut self) {
        self.chan.remove_msg_listener(self.msg_listener_hdl);
    }
}

/// Represents an event emitted by the [`WirelessDeviceStatusFeature`]
/// feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum WirelessDeviceStatusEvent {
    /// Is emitted whenever a device (re)connects to the host.
    ///
    /// This event is always enabled.
    StatusBroadcast(WirelessDeviceStatusBroadcast),
}

/// Represents the data of the [`WirelessDeviceStatusEvent::StatusBroadcast`]
/// event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct WirelessDeviceStatusBroadcast {
    /// The status the device reports to be in.
    pub status: WirelessDeviceStatus,

    /// The request the devices expresses towards the host.
    pub request: WirelessDeviceStatusRequest,

    /// The reason for the status broadcast.
    pub reason: WirelessDeviceStatusReason,
}

/// Represents a device status as reported in
/// [`WirelessDeviceStatusBroadcast::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WirelessDeviceStatus {
    Unknown = 0x00,
    Reconnection = 0x01,
}

/// Represents a request as reported in
/// [`WirelessDeviceStatusBroadcast::request`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WirelessDeviceStatusRequest {
    NoRequest = 0x00,
    SoftwareReconfigurationNeeded = 0x01,
}

/// Represents a broadcast reason as reported in
/// [`WirelessDeviceStatusBroadcast::reason`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WirelessDeviceStatusReason {
    Unknown = 0x00,
    PowerSwitchActivated = 0x01,
}
