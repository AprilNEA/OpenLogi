//! HID++ writes back to the device — DPI, SmartShift, lighting, and diagnostics.
//!
//! Each entry point takes a [`DeviceRoute`] and resolves it to an open channel
//! through `open_route_channel`, so the same call works whether the device is
//! behind a Bolt receiver or attached directly (USB cable / Bluetooth). Each
//! call re-enumerates and re-opens — fine at the frequency this is invoked
//! (once per slider release) — unless a [`SharedChannel`] from the capture
//! session is reused.

use std::sync::Arc;

use hidpp::{channel::HidppChannel, device::Device, feature::CreatableFeature};

use crate::route::{DeviceRoute, open_route_channel};

mod diagnostics;
mod dpi;
mod error;
mod fn_lock;
mod lighting;
mod shared;
mod smartshift;

pub use diagnostics::{FeatureEntry, ReprogControlEntry, dump_features, dump_reprog_controls};
pub use dpi::{DpiCapabilities, DpiInfo, get_dpi, get_dpi_info, set_dpi};
pub use error::{HidppFeatureErrorKind, HidppOperation, WriteError};
pub use fn_lock::set_fn_lock;
pub use lighting::{LightingMethod, set_keyboard_color, set_keyboard_color_with};
pub use shared::{
    SharedChannel, set_dpi_on, set_fn_lock_on, set_smartshift_on, toggle_smartshift_on,
};
pub use smartshift::{
    get_smartshift_status, set_smartshift, set_smartshift_sensitivity, toggle_smartshift,
};

pub(crate) use error::classify_hidpp_error;

/// Look up `F` on a device by HID++ feature ID, register it with
/// [`Device::add_feature`], and return the typed wrapper.
///
/// The direct lookup via `root().get_feature(id)` returns the assigned index
/// unconditionally; `add_feature` then attaches our wrapper to that index. This
/// keeps route-based write/read paths independent from full feature-table
/// enumeration and also works for feature wrappers that are not in the central
/// registry yet.
pub(crate) async fn open_feature<F: CreatableFeature + 'static>(
    device: &mut Device,
) -> Result<Arc<F>, WriteError> {
    let info = device
        .root()
        .get_feature(F::ID)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, F::ID))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex: F::ID })?;
    Ok(device.add_feature::<F>(info.index))
}

/// Boilerplate-eater: open the channel that reaches `route`, then run `f` once
/// with it. The caller addresses features at [`DeviceRoute::device_index`].
pub(crate) async fn with_route<F, Fut, T>(route: &DeviceRoute, f: F) -> Result<T, WriteError>
where
    F: FnOnce(Arc<HidppChannel>) -> Fut,
    Fut: std::future::Future<Output = Result<T, WriteError>>,
{
    match open_route_channel(route).await? {
        Some(channel) => f(channel).await,
        None => Err(WriteError::DeviceNotFound),
    }
}

#[cfg(test)]
mod tests;
