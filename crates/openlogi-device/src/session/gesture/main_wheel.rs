//! The main wheel's `0x2121` diversion for a capture session: arming moves its
//! movement reports from native HID to HID++, so the agent can post smoothed
//! scrolling in their place, and teardown hands them back to native HID.
//!
//! Logi Options+ 2.8 "Smooth scrolling" works the same way
//! (`devio_feature_x2121_gesture`): with Accessibility granted it
//! read-modify-writes the wheel mode to diverted, decodes each movement report,
//! and otherwise leaves the wheel native.

use std::num::NonZeroU8;
use std::sync::Arc;

use hidpp::channel::HidppChannel;
use hidpp::feature::CreatableFeature as _;
use hidpp::feature::hires_wheel::{
    HiResWheelEvent, HiResWheelFeature, WheelEventTarget, WheelMode, WheelResolution,
};
use hidpp::protocol::v20::Hidpp20Error;
use openlogi_core::config::ScrollResolution;

use super::CapturedInput;

/// The wheel mode a session writes along with the divert. The agent's own
/// resolution and inversion re-apply stands down while a session owns the
/// wheel, so these carry the same configured values: handing the wheel back
/// then leaves it exactly as that re-apply would have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MainWheelDivert {
    /// Resolution to select; `None` keeps the device's current one.
    pub resolution: Option<ScrollResolution>,
    /// Native inversion to leave in place for when the wheel is handed back
    /// (it has no effect on diverted reports); `None` keeps the device's
    /// current flag. Ignored on a wheel without the invert capability.
    pub inverted: Option<bool>,
}

/// The diverted main wheel of an armed session.
pub(super) struct ArmedMainWheel {
    pub(super) feature: HiResWheelFeature,
    pub(super) feature_index: u8,
    /// What arming wrote besides the target, already limited to what the
    /// wheel supports, so a wake re-arm writes the same mode.
    divert: MainWheelDivert,
    /// Units a high-resolution report carries per notch.
    pub(super) per_notch: NonZeroU8,
}

impl ArmedMainWheel {
    /// Open the wheel at `feature_index` and read what arming needs, without
    /// writing anything yet.
    pub(super) async fn open(
        chan: &Arc<HidppChannel>,
        slot: u8,
        feature_index: u8,
        requested: MainWheelDivert,
    ) -> Result<Self, Hidpp20Error> {
        let feature = HiResWheelFeature::new(Arc::clone(chan), slot, feature_index);
        let capabilities = feature.get_wheel_capabilities().await?;
        let divert = MainWheelDivert {
            resolution: requested.resolution,
            inverted: requested.inverted.filter(|_| capabilities.has_invert),
        };
        Ok(Self {
            feature,
            feature_index,
            divert,
            // A wheel reporting no multiplier cannot report in high
            // resolution; one unit per notch keeps its reports exact anyway.
            per_notch: NonZeroU8::new(capabilities.multiplier).unwrap_or(NonZeroU8::MIN),
        })
    }

    /// Divert the wheel's movement reports to HID++.
    pub(super) async fn divert(&self) -> Result<(), Hidpp20Error> {
        divert(&self.feature, self.divert).await
    }
}

/// Read-modify-write `feature`'s mode to diverted reporting with `divert`'s
/// fields, keeping every firmware-owned field, and check the echo. A wheel
/// already in that mode is left untouched.
pub(super) async fn divert(
    feature: &HiResWheelFeature,
    divert: MainWheelDivert,
) -> Result<(), Hidpp20Error> {
    let current = feature.get_wheel_mode().await?;
    let request = diverted_mode(current, divert);
    if documented(current) == documented(request) {
        return Ok(());
    }
    let echo = feature.set_wheel_mode(request).await?;
    applied(echo, request)
}

/// Hand `feature`'s movement reports back to native HID: read-modify-write the
/// target only, so the resolution and inversion arming left stay in place.
/// A wheel already reporting natively is left untouched.
pub(crate) async fn undivert(feature: &HiResWheelFeature) -> Result<(), Hidpp20Error> {
    let current = feature.get_wheel_mode().await?;
    if current.target == WheelEventTarget::Native {
        return Ok(());
    }
    let mut request = current;
    request.target = WheelEventTarget::Native;
    let echo = feature.set_wheel_mode(request).await?;
    applied(echo, request)
}

fn diverted_mode(current: WheelMode, divert: MainWheelDivert) -> WheelMode {
    let mut mode = current;
    mode.target = WheelEventTarget::Diverted;
    if let Some(resolution) = divert.resolution {
        mode.resolution = match resolution {
            ScrollResolution::Low => WheelResolution::Low,
            ScrollResolution::High => WheelResolution::High,
        };
    }
    if let Some(inverted) = divert.inverted {
        mode.inverted = inverted;
    }
    mode
}

/// A mode write took effect only if the echo matches the request in the
/// fields OpenLogi writes. The firmware owns the rest, as the agent's own
/// wheel-mode write also treats them.
fn applied(echo: WheelMode, request: WheelMode) -> Result<(), Hidpp20Error> {
    if documented(echo) == documented(request) {
        Ok(())
    } else {
        Err(Hidpp20Error::UnsupportedResponse)
    }
}

/// The documented mode fields: target, resolution and native inversion.
fn documented(mode: WheelMode) -> (WheelEventTarget, WheelResolution, bool) {
    (mode.target, mode.resolution, mode.inverted)
}

/// The input one diverted main-wheel movement report stands for: `None` for
/// a report that turned the wheel by nothing, which Options+ does not
/// dispatch either.
///
/// Only the vertical delta is kept. No wheel OpenLogi knows reports
/// horizontal movement over `0x2121` — tilt arrives as `0x1b04` controls —
/// and nothing documents which way its sign points, so scrolling it could
/// only guess.
pub(super) fn movement_input(
    event: HiResWheelEvent,
    per_notch: NonZeroU8,
) -> Option<CapturedInput> {
    let HiResWheelEvent::WheelMovement(movement) = event else {
        return None;
    };
    if movement.delta_vertical == 0 {
        return None;
    }
    Some(CapturedInput::MainWheel {
        delta: movement.delta_vertical,
        per_notch: match movement.resolution {
            WheelResolution::High => per_notch,
            _ => NonZeroU8::MIN,
        },
    })
}
