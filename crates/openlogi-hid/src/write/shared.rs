use std::sync::Arc;

use hidpp::channel::HidppChannel;

use crate::route::DeviceRoute;
use crate::smartshift::SmartShiftMode;

use super::WriteError;
use super::dpi::set_dpi_on_channel;
use super::smartshift::{set_smartshift_on_channel, toggle_smartshift_on_channel};

/// An open HID++ channel to a device, shared so DPI / SmartShift writes can
/// reuse the capture session's connection instead of re-enumerating and
/// opening a fresh channel each time (which costs ~100ms+).
///
/// Cheap to clone (an `Arc` plus the [`DeviceRoute`] it points at). Built by
/// the capture session via `SharedChannel::new` and stashed in a slot the
/// GUI's write path consults.
#[derive(Clone)]
pub struct SharedChannel {
    channel: Arc<HidppChannel>,
    route: DeviceRoute,
}

impl SharedChannel {
    /// Wrap an open channel that reaches `route`.
    #[must_use]
    pub(crate) fn new(channel: Arc<HidppChannel>, route: DeviceRoute) -> Self {
        Self { channel, route }
    }

    /// Whether this channel reaches `route` — so the write path only reuses it
    /// for the device it actually points at.
    #[must_use]
    pub fn matches(&self, route: &DeviceRoute) -> bool {
        self.route == *route
    }

    pub(crate) fn channel(&self) -> &Arc<HidppChannel> {
        &self.channel
    }

    pub(crate) fn device_index(&self) -> u8 {
        self.route.device_index()
    }
}

/// Write DPI on an already-open [`SharedChannel`] — the fast path that skips
/// enumeration and channel setup.
pub async fn set_dpi_on(shared: &SharedChannel, dpi: u16) -> Result<(), WriteError> {
    set_dpi_on_channel(&shared.channel, shared.route.device_index(), dpi).await
}

/// Toggle SmartShift on an already-open [`SharedChannel`].
pub async fn toggle_smartshift_on(shared: &SharedChannel) -> Result<SmartShiftMode, WriteError> {
    toggle_smartshift_on_channel(&shared.channel, shared.route.device_index()).await
}

/// Write a full SmartShift configuration on an already-open [`SharedChannel`]
/// — the fast path that skips enumeration and channel setup.
pub async fn set_smartshift_on(
    shared: &SharedChannel,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    set_smartshift_on_channel(
        &shared.channel,
        shared.route.device_index(),
        mode,
        auto_disengage,
        tunable_torque,
    )
    .await
}
