//! HID++ `Thumbwheel` (feature `0x2150`) — divert the MX-line horizontal thumb
//! wheel so its rotation and single-tap gesture arrive as HID++ events instead
//! of native HID scroll.
//!
//! The wheel only has two reporting modes — Native (HID scroll) or Diverted
//! (HID++ events) — there is no "report taps but keep scrolling" mode. So the
//! capture session diverts the wheel whenever the user's thumbwheel config
//! leaves its defaults (click bound, rotation rebound, or sensitivity changed),
//! and re-synthesises horizontal scroll from the rotation deltas to keep
//! scrolling working.
//!
//! `hidpp 0.2` ships no typed wrapper, so we re-implement the three functions
//! OpenLogi needs: `getThumbwheelInfo` (capabilities — notably whether the wheel
//! reports a single tap), `setThumbwheelReporting` (enter/leave diverted mode),
//! and decode the unsolicited `thumbwheelEvent`. Wire format from
//! `x2150_thumbwheel_v0.pdf`.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    nibble::U4,
    protocol::v20::{self, Hidpp20Error},
};

/// `Thumbwheel` HID++ feature ID.
pub const FEATURE_ID: u16 = 0x2150;

/// `getThumbwheelInfo` function ID.
const FN_GET_INFO: u8 = 0;
/// `setThumbwheelReporting` function ID.
const FN_SET_REPORTING: u8 = 2;

/// Reporting-mode value: native HID scroll.
const MODE_NATIVE: u8 = 0;
/// Reporting-mode value: diverted to HID++ events.
const MODE_DIVERTED: u8 = 1;

/// `c_single_tap` capability bit in `getThumbwheelInfo` byte 5.
const CAP_SINGLE_TAP: u8 = 0x08;
/// `single_tap` bit in `thumbwheelEvent` byte 5.
const EV_SINGLE_TAP: u8 = 0x08;
/// `proxy` bit in `thumbwheelEvent` byte 5.
const EV_PROXY: u8 = 0x04;
/// `touch` bit in `thumbwheelEvent` byte 5.
const EV_TOUCH: u8 = 0x02;

/// Where a `thumbwheelEvent` sits in the life cycle of one roll
/// (`thumbwheelEvent` byte 4).
///
/// This is what separates a deliberate tap from the tap the wheel's touch
/// sensor flags for the finger that rolled it: every report from `Start`
/// through `Stop` belongs to a roll, so a tap bit inside that span is an
/// artifact of the same contact. Only [`RotationStatus::Inactive`] means the
/// wheel is at rest and a tap is the user's own.
///
/// [`RotationStatus::Stop`] is why this field is read rather than inferred
/// from the report's own rotation. Observed on an MX Master 4 (Bolt) with
/// `examples/thumbwheel_trace`, one nudge of the wheel:
///
/// ```text
/// rot=  -1  byte4=0x02  byte5=0x02  touch=true   → Active
/// rot=   0  byte4=0x03  byte5=0x00  touch=false  → Stop
/// ```
///
/// The release reports no rotation at all, so rotation alone cannot tell it
/// from a tap on a settled wheel — and on a wheel that does support
/// `single_tap` that release is exactly where the artifact lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStatus {
    /// No rotation — the wheel is at rest.
    Inactive,
    /// The first rotation report of a roll.
    Start,
    /// A subsequent rotation report.
    Active,
    /// The roll ended: released, no touch.
    Stop,
}

impl RotationStatus {
    /// Decode `thumbwheelEvent` byte 4. Values outside the four the spec
    /// defines decode as [`RotationStatus::Inactive`] — a firmware that
    /// reports something else has said nothing about a roll, and the caller
    /// still has the report's own `rotation` to go on. Claiming a roll here
    /// instead would let one unrecognised value make the tap permanently
    /// undeliverable.
    #[must_use]
    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Start,
            2 => Self::Active,
            3 => Self::Stop,
            _ => Self::Inactive,
        }
    }

    /// Whether this report belongs to a roll — including its `Stop`, which
    /// carries no rotation of its own but is still the roll's own contact.
    #[must_use]
    pub fn is_rolling(self) -> bool {
        !matches!(self, Self::Inactive)
    }
}

/// Characteristics + capabilities returned by `getThumbwheelInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbwheelInfo {
    /// Ratchets per revolution in native (HID) mode.
    pub native_res: u16,
    /// Rotation increments per revolution in diverted (HID++) mode.
    pub diverted_res: u16,
    /// Original (un-inverted) positive rotation direction: `0` = positive toward
    /// the left/back of the device, `1` = positive toward the right/front.
    pub default_dir: u8,
    /// Whether the wheel reports a single-tap gesture — required to bind a click.
    pub supports_single_tap: bool,
}

/// A decoded `thumbwheelEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbwheelEvent {
    /// Relative wheel rotation since the last report (signed, in `diverted_res`
    /// increments). `+` follows `default_dir` unless inverted at divert time.
    pub rotation: i16,
    /// Where this report sits in the life cycle of a roll.
    pub rotation_status: RotationStatus,
    /// A single-tap gesture fired with this report.
    pub single_tap: bool,
    /// The user is touching the wheel.
    pub touch: bool,
    /// The user is in proximity of the wheel.
    pub proxy: bool,
}

/// Decode a channel message into a [`ThumbwheelEvent`] when it is the
/// unsolicited `0x2150` `thumbwheelEvent` (function `0`) for
/// `(device_index, feature_index)`.
///
/// Returns `None` for request responses (`software_id != 0`) and messages from
/// a different device or feature.
#[must_use]
pub fn decode_event(
    msg: &v20::Message,
    device_index: u8,
    feature_index: u8,
) -> Option<ThumbwheelEvent> {
    let header = msg.header();
    if header.device_index != device_index
        || header.feature_index != feature_index
        || header.software_id.to_lo() != 0
        || header.function_id.to_lo() != 0
    {
        return None;
    }
    let p = msg.extend_payload();
    Some(ThumbwheelEvent {
        rotation: i16::from_be_bytes([p[0], p[1]]),
        rotation_status: RotationStatus::from_byte(p[4]),
        single_tap: p[5] & EV_SINGLE_TAP != 0,
        touch: p[5] & EV_TOUCH != 0,
        proxy: p[5] & EV_PROXY != 0,
    })
}

/// `Thumbwheel` accessor bound to one device + resolved feature index.
///
/// Construct with the feature index from the device's root feature
/// (`get_feature(`[`FEATURE_ID`]`)`). Cheap to clone (an `Arc` plus two indices).
#[derive(Clone)]
pub struct Thumbwheel {
    chan: Arc<HidppChannel>,
    device_index: u8,
    feature_index: u8,
}

impl Thumbwheel {
    /// Bind the feature to `(device_index, feature_index)` on `chan`.
    #[must_use]
    pub fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            chan,
            device_index,
            feature_index,
        }
    }

    /// The feature index this accessor talks to — used to match unsolicited
    /// events in [`decode_event`].
    #[must_use]
    pub fn feature_index(&self) -> u8 {
        self.feature_index
    }

    /// Send a feature function call carrying a full long-message payload.
    async fn call(&self, function_id: u8, params: [u8; 16]) -> Result<[u8; 16], Hidpp20Error> {
        let response = self
            .chan
            .send_v20(v20::Message::Long(
                v20::MessageHeader {
                    device_index: self.device_index,
                    feature_index: self.feature_index,
                    function_id: U4::from_lo(function_id),
                    software_id: self.chan.get_sw_id(),
                },
                params,
            ))
            .await?;
        Ok(response.extend_payload())
    }

    /// Read the wheel's resolution and capabilities.
    pub async fn get_info(&self) -> Result<ThumbwheelInfo, Hidpp20Error> {
        let p = self.call(FN_GET_INFO, [0; 16]).await?;
        Ok(ThumbwheelInfo {
            native_res: u16::from_be_bytes([p[0], p[1]]),
            diverted_res: u16::from_be_bytes([p[2], p[3]]),
            default_dir: p[4] & 0x01,
            supports_single_tap: p[5] & CAP_SINGLE_TAP != 0,
        })
    }

    /// Enter (or leave) diverted reporting. `inv_dir` inverts the rotation sign
    /// relative to `default_dir`. Set `diverted = false` on teardown to hand
    /// native scrolling back to the firmware.
    pub async fn set_reporting(&self, diverted: bool, inv_dir: bool) -> Result<(), Hidpp20Error> {
        let mut params = [0u8; 16];
        params[0] = if diverted { MODE_DIVERTED } else { MODE_NATIVE };
        params[1] = u8::from(inv_dir);
        self.call(FN_SET_REPORTING, params).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(function_id: u8, software_id: u8, payload: [u8; 16]) -> v20::Message {
        v20::Message::Long(
            v20::MessageHeader {
                device_index: 2,
                feature_index: 6,
                function_id: U4::from_lo(function_id),
                software_id: U4::from_lo(software_id),
            },
            payload,
        )
    }

    #[test]
    fn decodes_rotation_and_tap() {
        let mut p = [0u8; 16];
        p[0..2].copy_from_slice(&(-7i16).to_be_bytes());
        p[4] = 2;
        p[5] = EV_SINGLE_TAP | EV_TOUCH;
        assert_eq!(
            decode_event(&event(0, 0, p), 2, 6),
            Some(ThumbwheelEvent {
                rotation: -7,
                rotation_status: RotationStatus::Active,
                single_tap: true,
                touch: true,
                proxy: false,
            })
        );
    }

    #[test]
    fn decodes_every_rotation_status() {
        let status = |byte| {
            let mut p = [0u8; 16];
            p[4] = byte;
            decode_event(&event(0, 0, p), 2, 6)
                .expect("event")
                .rotation_status
        };
        assert_eq!(status(0), RotationStatus::Inactive);
        assert_eq!(status(1), RotationStatus::Start);
        assert_eq!(status(2), RotationStatus::Active);
        assert_eq!(status(3), RotationStatus::Stop);
        assert_eq!(
            status(0xff),
            RotationStatus::Inactive,
            "an unrecognised value must not make the tap permanently undeliverable"
        );
    }

    /// The roll's own `Stop` reports no rotation — it is the release — so
    /// rotation alone cannot tell a settled wheel from one that just stopped.
    #[test]
    fn a_stop_report_is_still_part_of_the_roll() {
        assert!(RotationStatus::Stop.is_rolling());
        assert!(RotationStatus::Start.is_rolling());
        assert!(RotationStatus::Active.is_rolling());
        assert!(!RotationStatus::Inactive.is_rolling());
    }

    #[test]
    fn ignores_responses_and_foreign_messages() {
        let p = [0u8; 16];
        // software_id != 0 marks a request response, not an event.
        assert_eq!(decode_event(&event(0, 5, p), 2, 6), None);
        // Wrong feature index.
        assert_eq!(decode_event(&event(0, 0, p), 2, 9), None);
    }
}
