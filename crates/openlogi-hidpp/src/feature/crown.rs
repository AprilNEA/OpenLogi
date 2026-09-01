//! Implements the `Crown` feature (ID `0x4600`) for the MX Master's rotary
//! crown: reading its capabilities, configuring its mode (HID vs diverted,
//! free vs ratchet, timeouts), and receiving diverted rotation/touch/button
//! events.

pub mod event;

#[cfg(test)]
mod tests;

use std::num::NonZeroU8;

use num_enum::{IntoPrimitive, TryFromPrimitive};
use openlogi_hidpp_derive::Feature;

pub use event::{ActivityState, ButtonState, CrownEvent, CrownGesture, CrownUpdate, RotationState};

use crate::{
    feature::{EventSource, FeatureEndpoint},
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
///
/// The wire's `0` is the write-side "leave unchanged" sentinel, kept out of
/// this enum: [`SetCrownMode`] says "unchanged" with `None`, and a response
/// answering `0` for the current mode is [`Hidpp20Error::UnsupportedResponse`]
/// like any other value no mode can be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum ReportingMode {
    /// Events go to the native HID channel.
    Hid = 1,
    /// Events are diverted to HID++ (required for [`CrownEvent`]).
    Diverted = 2,
}

/// The crown's ratchet mode. The wire's `0` sentinel is kept out of the enum
/// for the same reason as [`ReportingMode`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, IntoPrimitive, TryFromPrimitive)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u8)]
pub enum RatchetMode {
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

/// The crown's current mode, from [`get_mode`](CrownFeature::get_mode).
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
/// `None` leaves a setting unchanged — encoded as the wire's `0` sentinel, so
/// "set it to 0" is unrepresentable rather than a silent no-op. The rotation
/// timeout is clipped to `0x40` by the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SetCrownMode {
    /// How events are reported, or `None` to leave unchanged.
    pub diverting: Option<ReportingMode>,
    /// Ratchet mode, or `None` to leave unchanged.
    pub ratchet_mode: Option<RatchetMode>,
    /// Rotation timeout in 10 ms steps, or `None` to leave unchanged.
    pub rotation_timeout: Option<NonZeroU8>,
    /// Short-long timeout in 10 ms steps, or `None` to leave unchanged.
    pub short_long_timeout: Option<NonZeroU8>,
    /// Double-tap speed in 10 ms steps, or `None` to leave unchanged.
    pub double_tap_speed: Option<NonZeroU8>,
}

/// Implements the `Crown` / `0x4600` feature.
#[derive(Feature)]
#[creatable(id = 0x4600, version = 0)]
pub struct CrownFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<CrownEvent>,
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

    /// Sets the crown's mode.
    ///
    /// Divert the crown ([`ReportingMode::Diverted`]) for [`CrownEvent`]s to be
    /// emitted.
    ///
    /// The spec's `SetMode` response merely echoes the request bytes — `None`
    /// sentinels included — so it carries no statement about the device's
    /// resulting mode and is deliberately not decoded (a `None` field's echo
    /// would even trip the strict [`CrownMode`] decoder). Read the mode back
    /// with [`get_mode`](Self::get_mode) when it is needed.
    pub async fn set_mode(&self, mode: SetCrownMode) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[..5].copy_from_slice(&[
            mode.diverting.map_or(0, u8::from),
            mode.ratchet_mode.map_or(0, u8::from),
            mode.rotation_timeout.map_or(0, NonZeroU8::get),
            mode.short_long_timeout.map_or(0, NonZeroU8::get),
            mode.double_tap_speed.map_or(0, NonZeroU8::get),
        ]);
        self.endpoint.call_long(2, args).await?;
        Ok(())
    }
}
