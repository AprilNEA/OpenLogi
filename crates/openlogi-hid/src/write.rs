//! HID++ writes back to the device — DPI and SmartShift.
//!
//! Each entry point takes a [`DeviceRoute`] and resolves it to an open channel
//! through [`open_route_channel`], so the same call works whether the device is
//! behind a Bolt receiver or attached directly (USB cable / Bluetooth). Each
//! call re-enumerates and re-opens — fine at the frequency this is invoked
//! (once per slider release) — unless a [`SharedChannel`] from the capture
//! session is reused.

use std::sync::Arc;

use hidpp::{channel::HidppChannel, device::Device, feature::CreatableFeature};
use thiserror::Error;
use tracing::debug;

use crate::adjustable_dpi::AdjustableDpiFeatureV0;
use crate::route::{DeviceRoute, open_route_channel};
use crate::smartshift::{SmartShiftFeatureV0, SmartShiftMode, SmartShiftStatus};

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
    #[error("no connected device matched the route")]
    DeviceNotFound,
    #[error("device at index {index:#04x} did not respond to HID++")]
    DeviceUnreachable { index: u8 },
    #[error("device does not expose HID++ feature {feature_hex:#06x}")]
    FeatureUnsupported { feature_hex: u16 },
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

/// Snapshot of one HID++ feature exposed by a device: protocol ID +
/// version. Returned by [`dump_features`] for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct FeatureEntry {
    pub id: u16,
    pub version: u8,
}

/// Enumerate every HID++ feature the device on `route` reports — used by
/// `openlogi diag features` to confirm which DPI / SmartShift / etc.
/// feature IDs a given peripheral actually exposes (e.g. some mice use
/// `0x2202 ExtendedAdjustableDpi` instead of `0x2201 AdjustableDpi`).
pub async fn dump_features(route: &DeviceRoute) -> Result<Vec<FeatureEntry>, WriteError> {
    use hidpp::feature::feature_set::FeatureSetFeature;
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        // The root feature exposes the FeatureSet (0x0001) at a fixed
        // address; we look it up directly rather than going through
        // `enumerate_features` so the iteration is observable.
        let feature_set_info = device
            .root()
            .get_feature(FeatureSetFeature::ID)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: FeatureSetFeature::ID,
            })?;
        let feature_set = device.add_feature::<FeatureSetFeature>(feature_set_info.index);
        let count = feature_set
            .count()
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
        let mut entries = Vec::with_capacity(usize::from(count));
        for i in 0..=count {
            let info = feature_set
                .get_feature(i)
                .await
                .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
            entries.push(FeatureEntry {
                id: info.id,
                version: info.version,
            });
        }
        Ok(entries)
    })
    .await
}

/// Look up `F` on a device by HID++ feature ID, register it with
/// [`Device::add_feature`], and return the typed wrapper.
///
/// We bypass [`Device::enumerate_features`] because hidpp 0.2's central
/// registry has `versions: &[]` for the features OpenLogi cares about
/// (`0x2201 AdjustableDpi`, `0x2202 ExtendedAdjustableDpi`). Calling
/// `enumerate_features` ends up _not_ registering them, so a subsequent
/// `device.get_feature::<F>()` looking up our own TypeId returns `None`
/// even when the device announces the feature ID. The direct lookup via
/// `root().get_feature(id)` returns the assigned index unconditionally;
/// `add_feature` then attaches our wrapper to that index.
async fn open_feature<F: CreatableFeature + 'static>(
    device: &mut Device,
) -> Result<Arc<F>, WriteError> {
    let info = device
        .root()
        .get_feature(F::ID)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?
        .ok_or(WriteError::FeatureUnsupported { feature_hex: F::ID })?;
    Ok(device.add_feature::<F>(info.index))
}

/// Read the device's current DPI on sensor 0 — companion to [`set_dpi`].
/// Used by `openlogi diag dpi` and any future Settings → Diagnostics
/// surface that wants to display the current value without writing.
pub async fn get_dpi(route: &DeviceRoute) -> Result<u16, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<AdjustableDpiFeatureV0>(&mut device).await?;
        feature
            .get_sensor_dpi(0)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Read the device's current SmartShift mode + sensitivity — companion to
/// [`toggle_smartshift`].
pub async fn get_smartshift_status(route: &DeviceRoute) -> Result<SmartShiftStatus, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<SmartShiftFeatureV0>(&mut device).await?;
        feature
            .get_status()
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))
    })
    .await
}

pub async fn set_dpi(route: &DeviceRoute, dpi: u16) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_dpi_on_channel(&channel, index, dpi).await
    })
    .await
}

/// The DPI write itself, on an already-open channel at HID++ `index`. Shared by
/// [`set_dpi`] (which opens a fresh channel) and [`set_dpi_on`] (which reuses
/// one).
async fn set_dpi_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    dpi: u16,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<AdjustableDpiFeatureV0>(&mut device).await?;
    feature
        .set_sensor_dpi(0, dpi)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    // Read back to confirm the firmware accepted the value. A mismatch is a
    // silent failure mode that's otherwise invisible — devices in low-power
    // states or with unsupported DPI ranges can ACK the write yet keep the old
    // value. We log a warning but still return Ok because the request reached
    // the device.
    if let Ok(actual) = feature.get_sensor_dpi(0).await {
        if actual == dpi {
            debug!(index, dpi, "wrote DPI (verified)");
        } else {
            tracing::warn!(
                index,
                requested = dpi,
                actual,
                "DPI write accepted but device reports a different value — \
                 likely out of the device's supported range"
            );
        }
    } else {
        debug!(index, dpi, "wrote DPI (read-back skipped)");
    }
    Ok(())
}

/// Toggle SmartShift mode (free ↔ ratchet) on `route`. Reads the current
/// mode first, then writes the opposite — keeps current sensitivity.
/// Returns the new mode written.
///
/// `FeatureUnsupported` when the device doesn't expose HID++ `0x2111`
/// (older Logi mice and most non-MX devices).
pub async fn toggle_smartshift(route: &DeviceRoute) -> Result<SmartShiftMode, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        toggle_smartshift_on_channel(&channel, index).await
    })
    .await
}

/// The SmartShift toggle itself, on an already-open channel at HID++ `index`.
/// Shared by [`toggle_smartshift`] and [`toggle_smartshift_on`].
async fn toggle_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<SmartShiftMode, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<SmartShiftFeatureV0>(&mut device).await?;
    let SmartShiftStatus { mode, sensitivity } = feature
        .get_status()
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    let next = mode.flipped();
    feature
        .set_status(next, sensitivity)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    debug!(index, ?next, "wrote SmartShift mode");
    Ok(next)
}

/// An open HID++ channel to a device, shared so DPI / SmartShift writes can
/// reuse the capture session's connection instead of re-enumerating and
/// opening a fresh channel each time (which costs ~100ms+).
///
/// Cheap to clone (an `Arc` plus the [`DeviceRoute`] it points at). Built by
/// the capture session via [`SharedChannel::new`] and stashed in a slot the
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

/// Boilerplate-eater: open the channel that reaches `route`, then run `f` once
/// with it. The caller addresses features at [`DeviceRoute::device_index`].
async fn with_route<F, Fut, T>(route: &DeviceRoute, f: F) -> Result<T, WriteError>
where
    F: FnOnce(Arc<HidppChannel>) -> Fut,
    Fut: std::future::Future<Output = Result<T, WriteError>>,
{
    match open_route_channel(route).await? {
        Some(channel) => f(channel).await,
        None => Err(WriteError::DeviceNotFound),
    }
}
