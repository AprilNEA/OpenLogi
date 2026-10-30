//! G-series `0x8110` button spy for diagnostics: which slot a press lands
//! on, the fact a model's `GamingLayout` is built from. The capture session
//! owns the spy while it runs host mode.

use std::sync::Arc;

use futures_lite::StreamExt;
use futures_lite::stream::Boxed;
use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        mouse_button_spy::{MouseButtonSpyEvent, MouseButtonSpyFeature},
    },
    protocol::v20::Hidpp20Error,
};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// A started `0x8110` button spy. Every press or release reports the mask of
/// held slots, bit `n` for slot `n`.
///
/// [`Self::stop`] stops the spy. Dropped without it, the mouse keeps
/// reporting until it reconnects.
pub struct ButtonSpyWatch {
    spy: Arc<MouseButtonSpyFeature>,
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

    /// The held-slot mask after the next press or release. `None` once the
    /// channel is gone.
    pub async fn next(&mut self) -> Option<u16> {
        loop {
            if let MouseButtonSpyEvent::Buttons { mask } = self.events.next().await? {
                return Some(mask);
            }
        }
    }

    /// Stop the spy.
    pub async fn stop(self) -> Result<(), WriteError> {
        self.spy.stop_spy().await.map_err(spy_error)
    }
}

/// Read the `0x8110` slot mapping of the mouse on `route` and start its
/// button spy. Leaves the onboard mode as it is.
pub async fn watch_button_spy(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ButtonSpyWatch, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        watch_button_spy_on_channel(&channel, index).await
    })
    .await
}

pub(super) async fn watch_button_spy_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<ButtonSpyWatch, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let spy = open_feature::<MouseButtonSpyFeature>(&mut device).await?;
    let count = spy.get_button_count().await.map_err(spy_error)?;
    let mapping = spy.get_mapping(count).await.map_err(spy_error)?;
    // Listen first: a press right after the start must not be lost.
    let events = spy.listen().boxed();
    spy.start_spy().await.map_err(spy_error)?;
    Ok(ButtonSpyWatch {
        spy,
        events,
        mapping,
    })
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
