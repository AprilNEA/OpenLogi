//! Implements the `Crown` feature (ID `0x4600`) for the MX Master's rotary
//! crown: reading its capabilities, configuring its mode (HID vs diverted,
//! free vs ratchet, timeouts), and receiving diverted rotation/touch/button
//! events.

pub mod event;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

pub use event::{ActivityState, ButtonState, CrownEvent, CrownGesture, CrownUpdate, RotationState};

use crate::{
    channel::{HidppChannel, MessageListenerGuard},
    event::EventEmitter,
    feature::{CreatableFeature, EmittingFeature, Feature, FeatureEndpoint, event_payload},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// Crown control capabilities, from [`get_info`](CrownFeature::get_info).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct CrownControlCapabilities: u8 {
        /// The crown has a button.
        const BUTTON = 1 << 0;
        /// The button reports long presses.
        const BUTTON_LONG_PRESS = 1 << 1;
        /// The ratchet is mechanized (no manual control).
        const MECHANIZED_RATCHET = 1 << 2;
        /// The rotation timeout is configurable.
        const ROTATION_TIMEOUT_CONFIGURABLE = 1 << 3;
        /// The short-long timeout is configurable.
        const SHORT_LONG_TIMEOUT_CONFIGURABLE = 1 << 4;
        /// The double-tap speed is configurable.
        const DOUBLE_TAP_SPEED_CONFIGURABLE = 1 << 5;
    }
}

bitflags::bitflags! {
    /// Crown sensor capabilities, from [`get_info`](CrownFeature::get_info).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct CrownSensorCapabilities: u8 {
        /// The crown has a proximity sensor.
        const PROXIMITY = 1 << 0;
        /// The crown has a touch sensor.
        const TOUCH = 1 << 1;
        /// The crown detects tap gestures.
        const TAP_GESTURE = 1 << 2;
        /// The crown detects double-tap gestures.
        const DOUBLE_TAP_GESTURE = 1 << 3;
    }
}

/// How crown events are reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ReportingMode {
    /// Leave the setting unchanged (write-only sentinel).
    NoChange = 0,
    /// Events go to the native HID channel.
    Hid = 1,
    /// Events are diverted to HID++ (required for [`CrownEvent`]).
    Diverted = 2,
}

/// The crown's ratchet mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RatchetMode {
    /// Leave the setting unchanged (write-only sentinel).
    NoChange = 0,
    /// Free-spinning mode.
    Free = 1,
    /// Ratchet (detented) mode.
    Ratchet = 2,
}

/// Crown info constants from [`get_info`](CrownFeature::get_info).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct CrownInfo {
    /// Control capabilities.
    pub controls: CrownControlCapabilities,
    /// Sensor capabilities.
    pub sensors: CrownSensorCapabilities,
    /// Number of slots per revolution.
    pub slots: u16,
    /// Number of ratchets per revolution.
    pub ratchets: u16,
}

/// The crown's mode, from [`get_mode`](CrownFeature::get_mode) and echoed by
/// [`set_mode`](CrownFeature::set_mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct CrownMode {
    /// How events are reported.
    pub diverting: ReportingMode,
    /// Ratchet mode.
    pub ratchet_mode: RatchetMode,
    /// Rotation timeout, in 10 ms steps.
    pub rotation_timeout: u8,
    /// Short-long press timeout, in 10 ms steps.
    pub short_long_timeout: u8,
    /// Double-tap speed, in 10 ms steps.
    pub double_tap_speed: u8,
}

impl CrownMode {
    fn from_payload(payload: &[u8; 16]) -> Result<Self, Hidpp20Error> {
        Ok(Self {
            diverting: ReportingMode::try_from(payload[0])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            ratchet_mode: RatchetMode::try_from(payload[1])
                .map_err(|_| Hidpp20Error::UnsupportedResponse)?,
            rotation_timeout: payload[2],
            short_long_timeout: payload[3],
            double_tap_speed: payload[4],
        })
    }
}

/// Mode settings to write with [`set_mode`](CrownFeature::set_mode).
///
/// Every field uses `0` / [`ReportingMode::NoChange`] / [`RatchetMode::NoChange`]
/// as a "leave unchanged" sentinel. The rotation timeout is clipped to `0x40`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SetCrownMode {
    /// How events are reported, or [`ReportingMode::NoChange`].
    pub diverting: ReportingMode,
    /// Ratchet mode, or [`RatchetMode::NoChange`].
    pub ratchet_mode: RatchetMode,
    /// Rotation timeout in 10 ms steps, or `0` to leave unchanged.
    pub rotation_timeout: u8,
    /// Short-long timeout in 10 ms steps, or `0` to leave unchanged.
    pub short_long_timeout: u8,
    /// Double-tap speed in 10 ms steps, or `0` to leave unchanged.
    pub double_tap_speed: u8,
}

/// Implements the `Crown` / `0x4600` feature.
pub struct CrownFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// The emitter used to publish decoded events.
    emitter: Arc<EventEmitter<CrownEvent>>,

    /// Removes the message listener when the feature is dropped.
    _msg_listener: MessageListenerGuard,
}

impl CreatableFeature for CrownFeature {
    const ID: u16 = 0x4600;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        let emitter = Arc::new(EventEmitter::new());

        let listener = chan.add_msg_listener_guarded({
            let emitter = Arc::clone(&emitter);

            move |raw, matched| {
                let Some((func, payload)) =
                    event_payload(raw, matched, device_index, feature_index)
                else {
                    return;
                };
                if let Some(event) = event::decode_event(func.to_lo(), &payload) {
                    emitter.emit(event);
                }
            }
        });

        Self {
            endpoint: FeatureEndpoint::new(chan, device_index, feature_index),
            emitter,
            _msg_listener: listener,
        }
    }
}

impl Feature for CrownFeature {}

impl EmittingFeature<CrownEvent> for CrownFeature {
    fn listen(&self) -> async_channel::Receiver<CrownEvent> {
        self.emitter.create_receiver()
    }
}

impl CrownFeature {
    /// Retrieves the crown's capabilities and slot/ratchet counts.
    pub async fn get_info(&self) -> Result<CrownInfo, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(CrownInfo {
            controls: CrownControlCapabilities::from_bits_retain(payload[0]),
            sensors: CrownSensorCapabilities::from_bits_retain(payload[1]),
            slots: u16::from_be_bytes([payload[2], payload[3]]),
            ratchets: u16::from_be_bytes([payload[4], payload[5]]),
        })
    }

    /// Retrieves the crown's current mode.
    pub async fn get_mode(&self) -> Result<CrownMode, Hidpp20Error> {
        let payload = self.endpoint.call(1, [0; 3]).await?.extend_payload();
        CrownMode::from_payload(&payload)
    }

    /// Sets the crown's mode and returns the resulting mode echoed by the device.
    ///
    /// Divert the crown ([`ReportingMode::Diverted`]) for [`CrownEvent`]s to be
    /// emitted.
    pub async fn set_mode(&self, mode: SetCrownMode) -> Result<CrownMode, Hidpp20Error> {
        let mut args = [0; 16];
        args[..5].copy_from_slice(&[
            mode.diverting.into(),
            mode.ratchet_mode.into(),
            mode.rotation_timeout,
            mode.short_long_timeout,
            mode.double_tap_speed,
        ]);
        let payload = self.endpoint.call_long(2, args).await?.extend_payload();
        CrownMode::from_payload(&payload)
    }
}
