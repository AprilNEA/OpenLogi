//! HID++ `MouseButtonSpy` (feature `0x8110`) — the G-line raw button-event tap
//! that stands in for `0x1b04` on mice which do not report it.
//!
//! Unlike `0x1b04` diversion this is a *tap*: starting it does not stop a
//! button's native HID report. Only buttons with no native report at all can
//! be dispatched from it without double-firing — see
//! [`crate::session::gesture`]'s `spy` submodule for the per-model map of
//! which buttons that is.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    feature::{CreatableFeature, EmittingFeature, mouse_button_spy as hidpp_spy},
    protocol::v20::Hidpp20Error,
};

pub use hidpp_spy::{MouseButtonIndex, MouseButtonMask, MouseButtonSpyEvent};

/// `MouseButtonSpy` HID++ feature ID.
pub const FEATURE_ID: u16 = 0x8110;

/// `MouseButtonSpy` accessor bound to one device + resolved feature index.
///
/// Construct with the feature index obtained from the device's root feature
/// (`get_feature(`[`FEATURE_ID`]`)`), then call the functions below. Cheap to
/// clone (an `Arc` plus two indices). Holding a clone alive keeps the
/// underlying feature's event listener registered on the channel — dropping
/// every clone stops delivery.
#[derive(Clone)]
pub struct MouseButtonSpy {
    inner: Arc<hidpp_spy::MouseButtonSpyFeature>,
    device_index: u8,
    feature_index: u8,
}

impl MouseButtonSpy {
    /// Bind the feature to `(device_index, feature_index)` on `chan`.
    #[must_use]
    pub fn new(chan: Arc<HidppChannel>, device_index: u8, feature_index: u8) -> Self {
        Self {
            inner: Arc::new(hidpp_spy::MouseButtonSpyFeature::new(
                chan,
                device_index,
                feature_index,
            )),
            device_index,
            feature_index,
        }
    }

    /// The feature index this accessor talks to.
    #[must_use]
    pub fn feature_index(&self) -> u8 {
        self.feature_index
    }

    /// The device index this accessor talks to.
    #[must_use]
    pub fn device_index(&self) -> u8 {
        self.device_index
    }

    /// Number of physical mouse buttons the spy can report.
    pub async fn get_button_count(&self) -> Result<u8, Hidpp20Error> {
        self.inner.get_mouse_button_count().await
    }

    /// Starts emitting button-state events (`startMouseButtonSpy`).
    pub async fn start_reporting(&self) -> Result<(), Hidpp20Error> {
        self.inner.start_spy().await
    }

    /// Stops emitting button-state events (`stopMouseButtonSpy`). Idempotent
    /// on the device — safe to call on a session that never started it.
    pub async fn stop_reporting(&self) -> Result<(), Hidpp20Error> {
        self.inner.stop_spy().await
    }

    /// Receiver for every decoded button-state event. A new receiver replays
    /// nothing already emitted — call before [`Self::start_reporting`] to
    /// avoid a race against the first event.
    #[must_use]
    pub fn listen(&self) -> async_channel::Receiver<MouseButtonSpyEvent> {
        self.inner.listen()
    }
}
