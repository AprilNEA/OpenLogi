use std::sync::Arc;

use hidpp::channel::HidppChannel;

use crate::route::DeviceRoute;
use crate::smartshift::SmartShiftMode;
use crate::smartshift::SmartShiftStatus;

use super::WriteError;
use super::dpi::{DpiInfo, get_dpi_info_on_channel, set_dpi_on_channel};
use super::fn_lock::set_fn_lock_on_channel;
use super::lighting::{LightingMethod, set_keyboard_color_with_on_channel};
use super::smartshift::{
    get_smartshift_status_on_channel, set_smartshift_on_channel, toggle_smartshift_on_channel,
};

/// An open HID++ channel to a device, shared so route-addressed reads and writes
/// can reuse an inventory- or capture-owned connection instead of
/// re-enumerating and opening a fresh channel each time (which costs ~100ms+).
///
/// Cheap to clone (an `Arc` plus the [`DeviceRoute`] it points at). Built by
/// the inventory registry or a standalone capture session.
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

/// Read current DPI and supported values on an already-open [`SharedChannel`].
pub async fn get_dpi_info_on(shared: &SharedChannel) -> Result<DpiInfo, WriteError> {
    get_dpi_info_on_channel(&shared.channel, shared.route.device_index()).await
}

/// Toggle SmartShift on an already-open [`SharedChannel`].
pub async fn toggle_smartshift_on(shared: &SharedChannel) -> Result<SmartShiftMode, WriteError> {
    toggle_smartshift_on_channel(&shared.channel, shared.route.device_index()).await
}

/// Read SmartShift mode and sensitivity on an already-open [`SharedChannel`].
pub async fn get_smartshift_status_on(
    shared: &SharedChannel,
) -> Result<SmartShiftStatus, WriteError> {
    get_smartshift_status_on_channel(&shared.channel, shared.route.device_index()).await
}

/// Write keyboard Fn-lock on an already-open [`SharedChannel`] — the fast
/// path that skips enumeration and channel setup.
pub async fn set_fn_lock_on(shared: &SharedChannel, on: bool) -> Result<(), WriteError> {
    set_fn_lock_on_channel(&shared.channel, shared.route.device_index(), on).await
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

/// Set a solid keyboard colour on an already-open [`SharedChannel`], using
/// [`LightingMethod::Auto`].
pub async fn set_keyboard_color_on(
    shared: &SharedChannel,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with_on(shared, LightingMethod::Auto, r, g, b).await
}

/// Set a solid keyboard colour on an already-open [`SharedChannel`] with an
/// explicit lighting method.
pub async fn set_keyboard_color_with_on(
    shared: &SharedChannel,
    method: LightingMethod,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    set_keyboard_color_with_on_channel(
        &shared.channel,
        shared.route.device_index(),
        method,
        r,
        g,
        b,
    )
    .await
}
