//! HID++ `0x8110 MouseButtonFilter` diagnostics: count, mapping, and a
//! button-spy watch that stops the stream on cancel or drop.

use std::future::Future;
use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature, EmittingFeature,
        mouse_button_filter::{MouseButtonFilterEvent, MouseButtonFilterFeature},
    },
};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

/// Snapshot of the device's `0x8110` button count and HID mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseButtonFilterDump {
    /// Spy-button slots the device reports.
    pub button_count: u8,
    /// One mapping byte per slot: `0` disabled, `1..=16` enabled HID codes.
    pub mapping: Vec<u8>,
}

/// Read the `0x8110` button count and current mapping.
pub async fn dump_mouse_button_filter(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<MouseButtonFilterDump, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        let feature = open_mouse_button_filter(&channel, index).await?;
        read_dump(&feature).await
    })
    .await
}

/// Start the button spy, deliver each bitmask to `on_event`, and stop the spy
/// when `cancel` resolves, the event stream ends, or this future is dropped.
///
/// `cancel` is typically Ctrl-C from the CLI. The spy is stopped on the happy
/// path and again from [`Drop`] if that call never runs, so the device is not
/// left streaming.
pub async fn watch_mouse_button_filter<F, C>(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    on_event: F,
    cancel: C,
) -> Result<(), WriteError>
where
    F: FnMut(u16),
    C: Future<Output = ()>,
{
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        watch_on_channel(&channel, index, on_event, cancel).await
    })
    .await
}

async fn open_mouse_button_filter(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<Arc<MouseButtonFilterFeature>, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    open_feature::<MouseButtonFilterFeature>(&mut device).await
}

async fn read_dump(
    feature: &MouseButtonFilterFeature,
) -> Result<MouseButtonFilterDump, WriteError> {
    // `get_mouse_button_mapping` reads the count first so it can slice the
    // mapping payload; reuse that length instead of a third HID round-trip.
    let mapping = feature.get_mouse_button_mapping().await.map_err(|e| {
        classify_hidpp_error(
            e,
            HidppOperation::DumpFeatures,
            MouseButtonFilterFeature::ID,
        )
    })?;
    let button_count =
        u8::try_from(mapping.len()).map_err(|_| WriteError::UnsupportedResponse {
            operation: HidppOperation::DumpFeatures,
            feature_hex: MouseButtonFilterFeature::ID,
        })?;
    Ok(MouseButtonFilterDump {
        button_count,
        mapping,
    })
}

async fn watch_on_channel<F, C>(
    channel: &Arc<HidppChannel>,
    index: u8,
    mut on_event: F,
    cancel: C,
) -> Result<(), WriteError>
where
    F: FnMut(u16),
    C: Future<Output = ()>,
{
    let feature = open_mouse_button_filter(channel, index).await?;
    let events = feature.listen();
    feature.start_mouse_button_spy().await.map_err(|e| {
        classify_hidpp_error(
            e,
            HidppOperation::DumpFeatures,
            MouseButtonFilterFeature::ID,
        )
    })?;

    let mut guard = SpyStopGuard {
        feature,
        stopped: false,
    };
    tokio::pin!(cancel);

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(MouseButtonFilterEvent::Buttons { mask }) => on_event(mask),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            () = &mut cancel => break,
        }
    }

    guard.stop().await
}

/// Stops the spy on drop if [`Self::stop`] was not awaited.
struct SpyStopGuard {
    feature: Arc<MouseButtonFilterFeature>,
    stopped: bool,
}

impl SpyStopGuard {
    async fn stop(&mut self) -> Result<(), WriteError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.feature.stop_mouse_button_spy().await.map_err(|e| {
            classify_hidpp_error(
                e,
                HidppOperation::DumpFeatures,
                MouseButtonFilterFeature::ID,
            )
        })
    }
}

impl Drop for SpyStopGuard {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let feature = Arc::clone(&self.feature);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            drop(handle.spawn(async move {
                let _ = feature.stop_mouse_button_spy().await;
            }));
        }
    }
}
