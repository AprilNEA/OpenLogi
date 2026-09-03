//! Implements `MouseButtonSpy` (feature `0x8110`).
//!
//! Logitech's own feature-ID table names this `"MouseButtonFilter"`, but
//! `cvuchener/hidpp`'s `hidpp20/IMouseButtonSpy.h` and Solaar's
//! `hidpp20_constants.py` agree on the name `MouseButtonSpy`, which also
//! matches what the feature does: a raw button-press event tap, independent
//! of `0x8100`'s onboard profile bindings. Per `IMouseButtonSpy.h`, a button
//! already bound to a native HID report keeps producing that report
//! alongside this feature's events — this is a tap, not a divert, and its
//! own `get`/`setMouseButtonMapping` functions (3/4) only suppress native
//! reports while the device is in [`OnboardMode::Host`](super::onboard_profiles::OnboardMode::Host).
//!
//! Only the read/event side is implemented here (`getMouseButtonCount`,
//! `start`/`stopMouseButtonSpy`, and the button-state event). functions 3/4
//! are not implemented — their byte layout is unverified against any source.

use std::num::NonZeroU8;

use openlogi_hidpp_derive::Feature;

use crate::{
    feature::{EventSource, FeatureEndpoint},
    protocol::v20::Hidpp20Error,
};

mod event;

pub use event::MouseButtonSpyEvent;

/// A 1-based mouse button ordinal (`1..=16`), as used by [`MouseButtonMask`]
/// and the (unimplemented) `get`/`setMouseButtonMapping` functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MouseButtonIndex(NonZeroU8);

impl MouseButtonIndex {
    /// The 1-based ordinal as a plain `u8`.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0.get()
    }
}

/// The 16-bit button-state bitmask carried by the spy's button event.
///
/// Bit 0 is button 1 — libratbag's convention for the sibling `0x8100`
/// button-binding bitmask; unverified against this feature's own events on
/// real hardware.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MouseButtonMask(u16);

impl MouseButtonMask {
    /// Wraps a raw 16-bit mask.
    #[must_use]
    pub fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// The mask's raw bits.
    #[must_use]
    pub fn bits(self) -> u16 {
        self.0
    }

    /// Every button index currently reported pressed.
    pub fn pressed(self) -> impl Iterator<Item = MouseButtonIndex> {
        (0u8..16).filter_map(move |bit| {
            (self.0 & (1 << bit) != 0)
                .then_some(bit + 1)
                .and_then(NonZeroU8::new)
                .map(MouseButtonIndex)
        })
    }
}

/// Implements the `MouseButtonSpy` / `0x8110` feature.
#[derive(Feature)]
#[creatable(id = 0x8110, version = 0)]
pub struct MouseButtonSpyFeature {
    endpoint: FeatureEndpoint,
    events: EventSource<MouseButtonSpyEvent>,
}

impl MouseButtonSpyFeature {
    /// Number of physical mouse buttons the spy can report
    /// (`getMouseButtonCount`, function 0).
    pub async fn get_mouse_button_count(&self) -> Result<u8, Hidpp20Error> {
        Ok(self.endpoint.call(0, [0; 3]).await?.extend_payload()[0])
    }

    /// Starts emitting button-state events (`startMouseButtonSpy`, function 1).
    pub async fn start_spy(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(1, [0; 3]).await?;
        Ok(())
    }

    /// Stops emitting button-state events (`stopMouseButtonSpy`, function 2).
    pub async fn stop_spy(&self) -> Result<(), Hidpp20Error> {
        self.endpoint.call(2, [0; 3]).await?;
        Ok(())
    }
}
