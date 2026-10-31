//! G-series `0x8110` button spy for diagnostics: which slot a press lands
//! on, the fact a model's `GamingLayout` is built from.
//!
//! In onboard mode the active profile decides what a button does, and a
//! button the profile leaves unbound can report nothing (a G903, #2178).
//! A watch therefore puts
//! a mouse with `0x8100` in host mode, where every slot reports, and returns
//! it to onboard mode when it stops. A mouse already in host mode belongs to
//! the program that switched it, so a watch refuses it.

use std::sync::Arc;

use futures_lite::StreamExt;
use futures_lite::stream::Boxed;
use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        mouse_button_spy::{MouseButtonSpyEvent, MouseButtonSpyFeature},
        onboard_profiles::{OnboardMode, OnboardProfilesFeature},
    },
    protocol::v20::Hidpp20Error,
};
use tracing::warn;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// Why a button spy watch did not start.
#[derive(Debug, thiserror::Error)]
pub enum ButtonSpyError {
    /// The mouse is in host mode, so the program that set it owns the spy.
    #[error("the mouse is in host mode, so another program owns its button spy")]
    HostMode,
    /// The mouse could not be read or refused a request.
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// A started `0x8110` button spy. Every press or release reports the mask of
/// held slots, bit `n` for slot `n`.
///
/// [`Self::stop`] stops the spy and returns the mouse to onboard mode.
/// Dropped without it, the mouse stays in host mode until it power-cycles.
pub struct ButtonSpyWatch {
    spy: Arc<MouseButtonSpyFeature>,
    /// Set when the watch put the mouse in host mode and owes the way back.
    onboard: Option<Arc<OnboardProfilesFeature>>,
    events: Boxed<MouseButtonSpyEvent>,
    mapping: Vec<u8>,
}

impl ButtonSpyWatch {
    /// The HID button each slot sends in host mode, `0` for none: entry `n`
    /// is slot `n`, one entry per slot the mouse reports.
    #[must_use]
    pub fn mapping(&self) -> &[u8] {
        &self.mapping
    }

    /// Whether the watch put the mouse in host mode. A mouse without
    /// `0x8100` has no host mode, and its spy runs as the mouse is.
    #[must_use]
    pub fn holds_host_mode(&self) -> bool {
        self.onboard.is_some()
    }

    /// The held-slot mask after the next press or release, or `None` once
    /// the report stream closes.
    ///
    /// A mouse that went away and one that nobody presses send the same
    /// nothing, so this waits for either. Bound the wait with a deadline;
    /// [`Self::stop`] then reports a mouse that no longer answers.
    pub async fn next(&mut self) -> Option<u16> {
        loop {
            if let MouseButtonSpyEvent::Buttons { mask } = self.events.next().await? {
                return Some(mask);
            }
        }
    }

    /// Stop the spy, then return the mouse to onboard mode. The mode goes
    /// back even when the stop fails: a mouse left in host mode keeps its
    /// unmapped buttons silent.
    pub async fn stop(self) -> Result<(), WriteError> {
        let stopped = self.spy.stop_spy().await.map_err(spy_error);
        if let Some(onboard) = &self.onboard {
            onboard
                .set_mode(OnboardMode::Onboard)
                .await
                .map_err(onboard_error)?;
        }
        stopped
    }
}

/// Read the `0x8110` slot mapping of the mouse on `route` and start its
/// button spy, in host mode when the mouse has one.
pub async fn watch_button_spy(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ButtonSpyWatch, ButtonSpyError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        Ok(watch_button_spy_on_channel(&channel, index).await)
    })
    .await?
}

pub(super) async fn watch_button_spy_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<ButtonSpyWatch, ButtonSpyError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let spy = open_feature::<MouseButtonSpyFeature>(&mut device).await?;
    let onboard = match open_feature::<OnboardProfilesFeature>(&mut device).await {
        Ok(onboard) => Some(onboard),
        Err(WriteError::FeatureUnsupported { .. }) => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(onboard) = &onboard
        && onboard.get_mode().await.map_err(onboard_error)? == OnboardMode::Host
    {
        return Err(ButtonSpyError::HostMode);
    }
    let count = spy.get_button_count().await.map_err(spy_error)?;
    let mapping = spy.get_mapping(count).await.map_err(spy_error)?;

    // A request whose reply is lost may still have switched the mouse, so
    // every failure from here on tries the way back.
    if let Some(onboard) = &onboard
        && let Err(error) = onboard.set_mode(OnboardMode::Host).await
    {
        restore_onboard(onboard).await;
        return Err(onboard_error(error).into());
    }
    // Listen first: a press right after the start must not be lost.
    let events = spy.listen().boxed();
    if let Err(error) = spy.start_spy().await {
        if let Some(onboard) = &onboard {
            restore_onboard(onboard).await;
        }
        return Err(spy_error(error).into());
    }
    Ok(ButtonSpyWatch {
        spy,
        onboard,
        events,
        mapping,
    })
}

/// The way back to onboard mode after a failed start. The start's own error
/// is the one the caller sees.
async fn restore_onboard(onboard: &OnboardProfilesFeature) {
    if let Err(error) = onboard.set_mode(OnboardMode::Onboard).await {
        warn!(?error, "leaving host mode after a failed spy start failed");
    }
}

fn onboard_error(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(
        error,
        HidppOperation::ReadOnboard,
        OnboardProfilesFeature::ID,
    )
}

fn spy_error(error: Hidpp20Error) -> WriteError {
    classify_hidpp_error(
        error,
        HidppOperation::DumpFeatures,
        MouseButtonSpyFeature::ID,
    )
}

#[cfg(test)]
mod tests;
