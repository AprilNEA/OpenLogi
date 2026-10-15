//! Implements the `WirelessDeviceStatus` feature (ID `0x1d4b`) that notifies
//! the host about device reconnections.

use std::sync::Arc;

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::{
    channel::HidppChannel,
    feature::{CreatableFeature, DecodeEvent, EmittingFeature, EventSource, Feature},
};

/// Implements the `WirelessDeviceStatus` / `0x1d4b` feature.
pub struct WirelessDeviceStatusFeature {
    /// Publishes decoded events to listeners.
    events: EventSource<WirelessDeviceStatusEvent>,
}

impl CreatableFeature for WirelessDeviceStatusFeature {
    const ID: u16 = 0x1d4b;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            events: EventSource::attach(&chan, device_index, feature_index),
        }
    }
}

impl Feature for WirelessDeviceStatusFeature {}

impl EmittingFeature<WirelessDeviceStatusEvent> for WirelessDeviceStatusFeature {
    fn listen(&self) -> async_channel::Receiver<WirelessDeviceStatusEvent> {
        self.events.listen()
    }
}

impl DecodeEvent for WirelessDeviceStatusEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // The reconnection broadcast is the only event and carries sub-id 0.
        if sub_id != 0 {
            return None;
        }

        // This broadcast is the device's (re)connection signal; an
        // unrecognised field value must not swallow it, so every field
        // decodes infallibly and carries unknown raw bytes.
        Some(WirelessDeviceStatusEvent::StatusBroadcast(
            WirelessDeviceStatusBroadcast {
                status: WirelessDeviceStatus::from(payload[0]),
                request: WirelessDeviceStatusRequest::from(payload[1]),
                reason: WirelessDeviceStatusReason::from(payload[2]),
            },
        ))
    }
}

impl WirelessDeviceStatusEvent {
    /// Decodes one event payload for this feature, or returns `None` for an
    /// unsupported event function.
    ///
    /// Consumers that already own a channel-level listener can use this
    /// without constructing a second [`WirelessDeviceStatusFeature`].
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WirelessDeviceStatus {
    /// Unknown wireless device status.
    Unknown = 0x00,
    /// Device is reconnecting.
    Reconnection = 0x01,
    /// A status value this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Represents a request as reported in
/// [`WirelessDeviceStatusBroadcast::request`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WirelessDeviceStatusRequest {
    /// No host action requested.
    NoRequest = 0x00,
    /// Host software must reconfigure the device.
    SoftwareReconfigurationNeeded = 0x01,
    /// A request value this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}

/// Represents a broadcast reason as reported in
/// [`WirelessDeviceStatusBroadcast::reason`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, FromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum WirelessDeviceStatusReason {
    /// Unknown broadcast reason.
    Unknown = 0x00,
    /// Broadcast was caused by the device power switch.
    PowerSwitchActivated = 0x01,
    /// A reason value this crate does not model; carries the raw byte.
    #[num_enum(catch_all)]
    Other(u8),
}
