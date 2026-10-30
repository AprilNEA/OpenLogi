//! G-series `0x8110` button spy for diagnostics: which slot a press lands
//! on, the fact a model's `GamingLayout` is built from. The program that
//! puts the mouse in host mode — the capture session, or G HUB — owns the
//! spy, so a watch neither starts nor stops it then.

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

/// What [`ButtonSpyWatch::stop`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSpyStop {
    /// The spy is stopped.
    Stopped,
    /// The mouse went into host mode during the watch. The spy now belongs
    /// to the program that switched it, so it stays on.
    HostOwned,
}

/// A started `0x8110` button spy. Every press or release reports the mask of
/// held slots, bit `n` for slot `n`.
///
/// [`Self::stop`] stops the spy. Dropped without it, the mouse keeps
/// reporting until it reconnects.
pub struct ButtonSpyWatch {
    spy: Arc<MouseButtonSpyFeature>,
    /// `None` on a mouse without `0x8100`, which has no host mode.
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

    /// Stop the spy, unless the mouse went into host mode during the watch.
    ///
    /// The mode is read before and after the stop. A program that took host
    /// mode between the two reads gets its spy back: the capture session
    /// sets host mode before it starts the spy, so a spy start the stop
    /// cancelled shows up as host mode on the second read.
    pub async fn stop(self) -> Result<ButtonSpyStop, WriteError> {
        if in_host_mode(self.onboard.as_deref()).await? {
            return Ok(ButtonSpyStop::HostOwned);
        }
        self.spy.stop_spy().await.map_err(spy_error)?;
        if in_host_mode(self.onboard.as_deref()).await? {
            self.spy.start_spy().await.map_err(spy_error)?;
            return Ok(ButtonSpyStop::HostOwned);
        }
        Ok(ButtonSpyStop::Stopped)
    }
}

/// Read the `0x8110` slot mapping of the mouse on `route` and start its
/// button spy. Leaves the onboard mode as it is, and refuses a mouse in host
/// mode.
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
    if in_host_mode(onboard.as_deref()).await? {
        return Err(ButtonSpyError::HostMode);
    }
    let count = spy.get_button_count().await.map_err(spy_error)?;
    let mapping = spy.get_mapping(count).await.map_err(spy_error)?;
    // Listen first: a press right after the start must not be lost.
    let events = spy.listen().boxed();
    spy.start_spy().await.map_err(spy_error)?;
    Ok(ButtonSpyWatch {
        spy,
        onboard,
        events,
        mapping,
    })
}

/// Whether the mouse is in host mode. A mouse without `0x8100` has none.
async fn in_host_mode(onboard: Option<&OnboardProfilesFeature>) -> Result<bool, WriteError> {
    let Some(onboard) = onboard else {
        return Ok(false);
    };
    let mode = onboard.get_mode().await.map_err(|error| {
        classify_hidpp_error(
            error,
            HidppOperation::ReadOnboard,
            OnboardProfilesFeature::ID,
        )
    })?;
    Ok(mode == OnboardMode::Host)
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
