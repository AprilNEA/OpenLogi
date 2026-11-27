//! Cursor placement on a Wayland session, through a virtual absolute pointer.
//!
//! A Wayland client cannot move the cursor, and a KWin script cannot either.
//! A pointer that reports absolute positions can: the compositor maps its
//! range onto the layout of all outputs and puts the cursor there. This device
//! only ever sends a position. It never presses a button and never reports
//! relative motion, so it cannot capture, block or move input beyond placing
//! the cursor.
//!
//! The device is created once, when the edge service starts, and removed when
//! it is dropped. Positions sent before the compositor has picked up a new
//! device are lost, so creating it per placement would drop the first one.

use std::io;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, UinputAbsSetup,
};
use openlogi_inject::uinput::{self, Device};

use crate::{DesktopPoint, DisplayBounds, WarpError};

/// The far end of both axes. The compositor maps `0..=RANGE` linearly onto the
/// bounding box of the outputs.
const RANGE: i32 = 65_535;

/// The virtual absolute pointer.
pub(super) struct Placement {
    device: VirtualDevice,
}

impl Placement {
    /// Create the device.
    ///
    /// # Errors
    ///
    /// The error from opening `/dev/uinput` or creating the device.
    pub(super) fn create() -> io::Result<Self> {
        // Only a left button exists, and it is never pressed. Without a
        // button the compositor would not treat the device as a pointer.
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_LEFT);
        let axis = |code| UinputAbsSetup::new(code, AbsInfo::new(0, 0, RANGE, 0, 0, 0));
        let device = uinput::builder(Device::PointerPlacement)?
            .with_keys(&keys)?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X))?
            .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y))?
            .build()?;
        Ok(Self { device })
    }

    /// Put the cursor at `to`, a point on one of `displays`.
    ///
    /// # Errors
    ///
    /// [`WarpError::OutsideDisplays`] when no display contains `to`, and
    /// [`WarpError::Rejected`] when the write to the device fails.
    pub(super) fn move_to(
        &mut self,
        to: DesktopPoint,
        displays: &[DisplayBounds],
    ) -> Result<(), WarpError> {
        let (x, y) = absolute_position(to, displays)?;
        self.device
            .emit(&[
                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, x),
                InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_Y.0, y),
            ])
            .map_err(WarpError::Rejected)
    }
}

/// Where `to` falls in the device's `0..=RANGE` square, which the compositor
/// stretches over the bounding box of `displays`.
fn absolute_position(
    to: DesktopPoint,
    displays: &[DisplayBounds],
) -> Result<(i32, i32), WarpError> {
    if !displays.iter().any(|display| display.contains(to)) {
        return Err(WarpError::OutsideDisplays);
    }
    let left = displays.iter().map(|d| d.x).fold(f64::INFINITY, f64::min);
    let top = displays.iter().map(|d| d.y).fold(f64::INFINITY, f64::min);
    let right = displays
        .iter()
        .map(|d| d.x + d.width)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = displays
        .iter()
        .map(|d| d.y + d.height)
        .fold(f64::NEG_INFINITY, f64::max);
    Ok((
        scale(to.x - left, right - left),
        scale(to.y - top, bottom - top),
    ))
}

/// `offset` of `span` as a point in `0..=RANGE`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the rounded value is clamped to 0..=RANGE before conversion"
)]
fn scale(offset: f64, span: f64) -> i32 {
    (offset / span * f64::from(RANGE))
        .round()
        .clamp(0.0, f64::from(RANGE)) as i32
}

#[cfg(test)]
mod tests;
