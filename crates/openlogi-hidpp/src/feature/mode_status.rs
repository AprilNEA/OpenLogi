//! Implements `ModeStatus` (feature `0x8090`).

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{DecodeEvent, EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

bitflags::bitflags! {
    /// The first mode-status byte.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ModeStatus0: u8 {
        /// Performance mode. When unset, the device is in endurance mode.
        const PERFORMANCE = 1 << 0;
    }
}

bitflags::bitflags! {
    /// Capabilities reported by `ModeStatus`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize))]
    pub struct ModeStatusCapabilities: u16 {
        /// A hardware switch can change the mode bit.
        const HARDWARE_SWITCH = 1 << 0;
        /// Software can change the mode bit.
        const SOFTWARE_SWITCH = 1 << 1;
    }
}

/// Current mode-status bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ModeStatus {
    /// Primary status bits.
    pub status0: ModeStatus0,
    /// Secondary status byte, reserved by v1 but preserved for callers.
    pub status1: u8,
}

/// A mode-status update request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ModeStatusChange {
    /// Desired primary status bits.
    pub status0: ModeStatus0,
    /// Desired secondary status byte.
    pub status1: u8,
    /// Primary changed-bit mask.
    pub changed_mask0: ModeStatus0,
    /// Secondary changed-bit mask.
    pub changed_mask1: u8,
}

/// Implements the `ModeStatus` / `0x8090` feature.
#[derive(Feature)]
#[creatable(id = 0x8090, version = 1)]
pub struct ModeStatusFeature {
    /// The endpoint this feature talks to.
    endpoint: FeatureEndpoint,

    /// Publishes decoded events to listeners.
    events: EventSource<ModeStatusEvent>,
}

impl ModeStatusFeature {
    /// Retrieves the current mode status.
    pub async fn get_mode_status(&self) -> Result<ModeStatus, Hidpp20Error> {
        let payload = self.endpoint.call(0, [0; 3]).await?.extend_payload();
        Ok(ModeStatus {
            status0: ModeStatus0::from_bits_retain(payload[0]),
            status1: payload[1],
        })
    }

    /// Sets selected mode-status bits.
    pub async fn set_mode_status(&self, change: ModeStatusChange) -> Result<(), Hidpp20Error> {
        let mut args = [0; 16];
        args[0] = change.status0.bits();
        args[1] = change.status1;
        args[2] = change.changed_mask0.bits();
        args[3] = change.changed_mask1;

        self.endpoint.call_long(1, args).await?;
        Ok(())
    }

    /// Enables or disables performance mode.
    pub async fn set_performance_mode(&self, enabled: bool) -> Result<(), Hidpp20Error> {
        let status0 = if enabled {
            ModeStatus0::PERFORMANCE
        } else {
            ModeStatus0::empty()
        };
        self.set_mode_status(ModeStatusChange {
            status0,
            status1: 0,
            changed_mask0: ModeStatus0::PERFORMANCE,
            changed_mask1: 0,
        })
        .await
    }

    /// Retrieves device capabilities for mode switching.
    ///
    /// The capability bits live in the reply's first byte: a G305 answers
    /// `0x02` there (software switch only, matching its software-togglable
    /// mode), which a big-endian two-byte read would shift into the high byte
    /// and report as no capabilities at all. Decoded little-endian so byte 0
    /// carries bits 0-7 as observed on hardware while a second byte, if a
    /// future device uses one, still surfaces.
    pub async fn get_device_config(&self) -> Result<ModeStatusCapabilities, Hidpp20Error> {
        let payload = self.endpoint.call(2, [0; 3]).await?.extend_payload();
        Ok(ModeStatusCapabilities::from_bits_retain(
            u16::from_le_bytes([payload[0], payload[1]]),
        ))
    }
}

impl DecodeEvent for ModeStatusEvent {
    fn decode(sub_id: u8, payload: &[u8; 16]) -> Option<Self> {
        // The mode-status broadcast is the only event and carries sub-id 0.
        if sub_id != 0 {
            return None;
        }

        // Every field decodes infallibly (`from_bits_retain` keeps unknown
        // bits): a broadcast flipping a bit this crate does not model yet must
        // still reach listeners, mirroring `wireless_device_status`.
        Some(ModeStatusEvent::StatusBroadcast(ModeStatusBroadcast {
            status: ModeStatus {
                status0: ModeStatus0::from_bits_retain(payload[0]),
                status1: payload[1],
            },
            changed_mask0: ModeStatus0::from_bits_retain(payload[2]),
            changed_mask1: payload[3],
        }))
    }
}

impl ModeStatusEvent {
    /// Decodes one event payload for this feature, or returns `None` for an
    /// unsupported event function.
    ///
    /// Consumers that already own a channel-level listener can use this
    /// without constructing a second [`ModeStatusFeature`].
    #[must_use]
    pub fn decode(function_id: u8, payload: &[u8; 16]) -> Option<Self> {
        <Self as DecodeEvent>::decode(function_id, payload)
    }
}

/// Represents an event emitted by the [`ModeStatusFeature`] feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum ModeStatusEvent {
    /// Is emitted whenever the mode-status bits change — observed after a
    /// successful `setModeStatus` and on power-on (alongside the `0x1d4b`
    /// reconnection broadcast).
    StatusBroadcast(ModeStatusBroadcast),
}

/// Represents the data of the [`ModeStatusEvent::StatusBroadcast`] event.
///
/// The payload layout mirrors the `setModeStatus` argument order (`status0`,
/// `status1`, `changed_mask0`, `changed_mask1`). Reverse-engineered from G305
/// captures rather than read from a spec: `01 00 01` right after a successful
/// set, `00 00 01` on power-on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ModeStatusBroadcast {
    /// The mode-status bytes after the change.
    pub status: ModeStatus,
    /// Primary changed-bit mask.
    pub changed_mask0: ModeStatus0,
    /// Secondary changed-bit mask.
    pub changed_mask1: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_g305_post_set_broadcast() {
        // Observed on a G305 right after a successful setModeStatus.
        let mut payload = [0u8; 16];
        payload[..3].copy_from_slice(&[0x01, 0x00, 0x01]);

        let event = ModeStatusEvent::decode(0, &payload).expect("event 0 decodes");
        let ModeStatusEvent::StatusBroadcast(broadcast) = event;
        assert!(broadcast.status.status0.contains(ModeStatus0::PERFORMANCE));
        assert_eq!(broadcast.status.status1, 0);
        assert_eq!(broadcast.changed_mask0, ModeStatus0::PERFORMANCE);
        assert_eq!(broadcast.changed_mask1, 0);
    }

    #[test]
    fn decodes_the_power_on_broadcast_with_the_bit_cleared() {
        // Observed on a G305 power-on: the performance bit reported cleared.
        let mut payload = [0u8; 16];
        payload[..3].copy_from_slice(&[0x00, 0x00, 0x01]);

        let event = ModeStatusEvent::decode(0, &payload).expect("event 0 decodes");
        let ModeStatusEvent::StatusBroadcast(broadcast) = event;
        assert!(!broadcast.status.status0.contains(ModeStatus0::PERFORMANCE));
        assert_eq!(broadcast.changed_mask0, ModeStatus0::PERFORMANCE);
    }

    #[test]
    fn other_functions_do_not_decode() {
        assert_eq!(ModeStatusEvent::decode(1, &[0; 16]), None);
    }
}
