//! HID++ `SmartShift Enhanced` (feature `0x2111`) — wheel ratchet ↔
//! free-spin control with sensitivity threshold.
//!
//! `hidpp 0.2` ships a typed wrapper for the original `0x2110 SmartShift`
//! at function IDs `0` / `1`. The "Enhanced" variant `0x2111` (MX Master
//! 3 / 3S / 4 and most current MX-line devices) shifts the call table by
//! one slot — `0` is a capability query, `1` is the status read, `2` is
//! the status write. Using `0x2110`'s function IDs against a `0x2111`
//! device hits the wrong functions and the device silently keeps its
//! previous state.
//!
//! Mode encoding (consistent across 0x2110 / 0x2111):
//! - `wheelMode` `1` = free-spin (no ratchet, infinite scroll), `2` =
//!   ratchet (clicky).
//! - `autoDisengage` `0x01`–`0xFE` = the wheel speed (in 0.25 turn/s steps)
//!   past which a ratchet-mode wheel releases into free-spin — i.e. the
//!   "SmartShift" threshold. `0xFF` keeps the ratchet engaged permanently
//!   (never auto-switches). See [`AUTO_DISENGAGE_PERMANENT`].

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    feature::{CreatableFeature, Feature},
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// SmartShift mode values understood by the firmware. `Free` = free-spin,
/// `Ratchet` = clicky / smartshift-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartShiftMode {
    Free,
    Ratchet,
}

impl SmartShiftMode {
    /// Wire byte for the `setRatchetControlMode` request.
    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            SmartShiftMode::Free => 1,
            SmartShiftMode::Ratchet => 2,
        }
    }

    /// Inverse of [`Self::as_byte`]. `None` for reserved values (0 / 3 /
    /// future). Callers fall back to whatever they consider sane.
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Free),
            2 => Some(Self::Ratchet),
            _ => None,
        }
    }

    /// The opposite mode — used by [`crate::write::toggle_smartshift`].
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            Self::Free => Self::Ratchet,
            Self::Ratchet => Self::Free,
        }
    }
}

/// `autoDisengage` value that keeps the ratchet engaged permanently — the
/// wheel never auto-releases into free-spin, regardless of speed. Any other
/// value (`0x01`–`0xFE`) is a SmartShift speed threshold.
pub const AUTO_DISENGAGE_PERMANENT: u8 = 0xff;

/// Snapshot returned from [`SmartShiftFeatureV0::get_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmartShiftStatus {
    pub mode: SmartShiftMode,
    /// SmartShift speed threshold: `0x01`–`0xFE` in 0.25 turn/s steps (higher
    /// = harder to flip into free-spin while scrolling; Logitech defaults to
    /// ~16 on the MX line), or [`AUTO_DISENGAGE_PERMANENT`] for a permanently
    /// engaged ratchet.
    pub auto_disengage: u8,
    /// Tunable-torque force as a percentage (`1`–`100`) of the device's max
    /// force, or `0` when the device doesn't support tunable torque. Read back
    /// and re-sent unchanged so adjusting the mode or threshold doesn't
    /// disturb the wheel's resistance.
    pub tunable_torque: u8,
}

/// `SmartShift` / `0x2111` feature, version 0+.
#[derive(Clone)]
pub struct SmartShiftFeatureV0 {
    chan: Arc<HidppChannel>,
    device_index: u8,
    feature_index: u8,
}

impl CreatableFeature for SmartShiftFeatureV0 {
    const ID: u16 = 0x2111;
    const STARTING_VERSION: u8 = 0;

    fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }
}

impl Feature for SmartShiftFeatureV0 {}

/// `0x2111` function ID for `getStatus` — returns mode + current
/// sensitivity + default sensitivity. Different from `0x2110` which uses
/// function `0` for the same purpose.
const FUNCTION_GET_STATUS: u8 = 1;
/// `0x2111` function ID for `setStatus` — accepts mode + sensitivity +
/// defaultSensitivity. `0x2110` uses function `1` here.
const FUNCTION_SET_STATUS: u8 = 2;

impl SmartShiftFeatureV0 {
    /// Read the current `wheelMode` + `autoDisengage` + `currentTunableTorque`.
    /// Reserved mode bytes fall back to [`SmartShiftMode::Ratchet`] because
    /// that's the "safe" / clicky behaviour most users expect.
    pub async fn get_status(&self) -> Result<SmartShiftStatus, Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(FUNCTION_GET_STATUS),
                    software_id: self.chan.get_sw_id(),
                },
                [0x00, 0x00, 0x00],
            ))
            .await?;
        let payload = response.extend_payload();
        let mode = SmartShiftMode::from_byte(payload[0]).unwrap_or(SmartShiftMode::Ratchet);
        Ok(SmartShiftStatus {
            mode,
            auto_disengage: payload[1],
            tunable_torque: payload.get(2).copied().unwrap_or(0),
        })
    }

    /// Write a new `wheelMode` + `autoDisengage` + `currentTunableTorque`. The
    /// firmware stores all three persistently in the device's NVM, so callers
    /// should read the current `tunable_torque` (and any field they don't mean
    /// to change) via [`Self::get_status`] and re-send it here.
    pub async fn set_status(
        &self,
        mode: SmartShiftMode,
        auto_disengage: u8,
        tunable_torque: u8,
    ) -> Result<(), Hidpp20Error> {
        let _ = self
            .chan
            .send_v20(v20::Message::Short(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(FUNCTION_SET_STATUS),
                    software_id: self.chan.get_sw_id(),
                },
                [mode.as_byte(), auto_disengage, tunable_torque],
            ))
            .await?;
        Ok(())
    }
}
