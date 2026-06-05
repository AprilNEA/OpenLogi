//! HID++ writes back to the device — DPI and SmartShift.
//!
//! Each entry point takes a [`DeviceRoute`] and resolves it to an open channel
//! through [`open_route_channel`], so the same call works whether the device is
//! behind a Bolt receiver or attached directly (USB cable / Bluetooth). Each
//! call re-enumerates and re-opens — fine at the frequency this is invoked
//! (once per slider release) — unless a [`SharedChannel`] from the capture
//! session is reused.

use std::sync::Arc;

use async_hid::AsyncHidWrite;
use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::CreatableFeature,
    feature::adjustable_dpi::AdjustableDpiFeature,
    protocol::v20::{ErrorType, Hidpp20Error},
};
use thiserror::Error;
use tracing::debug;

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
    #[error("device returned no supported DPI values")]
    EmptyDpiList,
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

/// Supported DPI values reported by a device's HID++ AdjustableDpi feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpiCapabilities {
    values: Vec<u16>,
}

impl DpiCapabilities {
    /// Build capabilities from a device-reported DPI list. Values are sorted
    /// and deduplicated so callers can rely on stable ordering.
    pub fn new(mut values: Vec<u16>) -> Result<Self, WriteError> {
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            return Err(WriteError::EmptyDpiList);
        }
        Ok(Self { values })
    }

    /// All supported DPI values, sorted ascending.
    #[must_use]
    pub fn values(&self) -> &[u16] {
        &self.values
    }

    /// Minimum supported DPI.
    #[must_use]
    pub fn min(&self) -> u16 {
        self.values[0]
    }

    /// Maximum supported DPI.
    #[must_use]
    pub fn max(&self) -> u16 {
        self.values[self.values.len() - 1]
    }

    /// Whether `dpi` is exactly supported by the device.
    #[must_use]
    pub fn contains(&self, dpi: u16) -> bool {
        self.values.binary_search(&dpi).is_ok()
    }

    /// The supported DPI nearest to `dpi`.
    #[must_use]
    pub fn nearest(&self, dpi: u32) -> u16 {
        let mut nearest = self.values[0];
        let mut best_delta = u32::from(nearest).abs_diff(dpi);
        for &candidate in &self.values[1..] {
            let delta = u32::from(candidate).abs_diff(dpi);
            if delta < best_delta {
                nearest = candidate;
                best_delta = delta;
            }
        }
        nearest
    }

    /// Snap `dpi` to the nearest supported value, widened to `u32` for UI math.
    /// The single home for "round a DPI onto this device's grid" — callers that
    /// hold an `Option<DpiCapabilities>` should `map_or(dpi, |c| c.snap(dpi))`.
    #[must_use]
    pub fn snap(&self, dpi: u32) -> u32 {
        u32::from(self.nearest(dpi))
    }

    /// Best-effort step size for UI widgets that need a single increment.
    /// Returns the smallest positive gap between adjacent reported values.
    #[must_use]
    pub fn step_hint(&self) -> u16 {
        self.values
            .windows(2)
            .filter_map(|pair| pair[1].checked_sub(pair[0]))
            .filter(|step| *step > 0)
            .min()
            .unwrap_or(1)
    }

    /// A supported value different from `current`, for diagnostic write tests.
    #[must_use]
    pub fn adjacent_test_target(&self, current: u16) -> Option<u16> {
        if self.values.len() < 2 {
            return None;
        }
        match self.values.binary_search(&current) {
            Ok(index) if index + 1 < self.values.len() => Some(self.values[index + 1]),
            Ok(index) if index > 0 => Some(self.values[index - 1]),
            Ok(_) => None,
            Err(index) if index < self.values.len() => Some(self.values[index]),
            Err(_) => self.values.last().copied(),
        }
        .filter(|target| *target != current)
    }
}

/// Current DPI plus the supported values reported by the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpiInfo {
    /// DPI currently configured on sensor 0.
    pub current: u16,
    /// Supported values reported by the device for sensor 0.
    pub capabilities: DpiCapabilities,
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
/// The direct lookup via `root().get_feature(id)` returns the assigned index
/// unconditionally; `add_feature` then attaches our wrapper to that index. This
/// keeps route-based write/read paths independent from full feature-table
/// enumeration and also works for feature wrappers that are not in the central
/// registry yet.
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
        let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
        feature
            .get_sensor_dpi(0)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))
    })
    .await
}

/// Classify a HID++ error from the AdjustableDpi functions. A device that
/// announces `0x2201` but rejects a function (`Unsupported` /
/// `InvalidFunctionId`) or returns a structurally invalid DPI list
/// (`UnsupportedResponse`) will keep doing so, so these map to the permanent
/// [`WriteError::FeatureUnsupported`]; channel/timeout errors stay transient
/// [`WriteError::Hidpp`] so callers may retry.
fn classify_dpi_error(error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Feature(ErrorType::Unsupported | ErrorType::InvalidFunctionId)
        | Hidpp20Error::UnsupportedResponse => WriteError::FeatureUnsupported {
            feature_hex: AdjustableDpiFeature::ID,
        },
        other => WriteError::Hidpp(format!("{other:?}")),
    }
}

/// Read the current DPI and the supported DPI values for sensor 0 in one
/// route/channel session.
pub async fn get_dpi_info(route: &DeviceRoute) -> Result<DpiInfo, WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        let mut device = Device::new(Arc::clone(&channel), index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable { index })?;
        let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
        let sensor_count = feature
            .get_sensor_count()
            .await
            .map_err(classify_dpi_error)?;
        if sensor_count == 0 {
            // The device claims AdjustableDpi but exposes no sensor — it cannot
            // report DPI, and that won't change on retry.
            return Err(WriteError::FeatureUnsupported {
                feature_hex: AdjustableDpiFeature::ID,
            });
        }
        let current = feature
            .get_sensor_dpi(0)
            .await
            .map_err(classify_dpi_error)?;
        let values = feature
            .get_sensor_dpi_list(0)
            .await
            .map_err(classify_dpi_error)?;
        Ok(DpiInfo {
            current,
            capabilities: DpiCapabilities::new(values)?,
        })
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

/// HID++ `PerKeyLighting` feature — drives the per-key RGB on wired G-series
/// keyboards. Its feature *index* varies per device, so it's resolved at runtime.
const PER_KEY_LIGHTING_FEATURE: u16 = 0x8080;

/// Set a keyboard's per-key RGB to a solid `(r, g, b)` colour via HID++
/// `PerKeyLighting` (`0x8080`): stream every key's colour in 64-byte `0x12`
/// "set group keys" frames, then commit the frame.
///
/// The `0x8080` feature index is resolved per device (it differs across the
/// G-series), so this isn't tied to the G513. `FeatureUnsupported` when the
/// device exposes no `0x8080` (e.g. a non-RGB peripheral).
pub async fn set_keyboard_color(
    route: &DeviceRoute,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), WriteError> {
    // Resolve this device's runtime feature index for 0x8080 over the HID++
    // channel; the per-key frames are then sent as raw 0x12 reports (which the
    // typed channel can't model) on a freshly opened writer.
    let device_index = route.device_index();
    let feature_index = with_route(route, move |channel| async move {
        let device = Device::new(Arc::clone(&channel), device_index)
            .await
            .map_err(|_| WriteError::DeviceUnreachable {
                index: device_index,
            })?;
        let info = device
            .root()
            .get_feature(PER_KEY_LIGHTING_FEATURE)
            .await
            .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?
            .ok_or(WriteError::FeatureUnsupported {
                feature_hex: PER_KEY_LIGHTING_FEATURE,
            })?;
        Ok(info.index)
    })
    .await?;

    let Some(mut writer) = crate::transport::open_route_writer(route).await? else {
        return Err(WriteError::DeviceNotFound);
    };
    // Each 64-byte `0x12` "set group keys" packet carries up to 14
    // `(keyID, R, G, B)` entries; keyIDs are HID usage codes. Cover the whole
    // keyboard usage range (incl. modifiers at `0xe0..`) so every key lights,
    // then commit the frame.
    let key_ids: Vec<u8> = (0x00u8..=0xe8).collect();
    for chunk in key_ids.chunks(14) {
        let mut rep = vec![0u8; 64];
        rep[0] = 0x12;
        rep[1] = device_index;
        rep[2] = feature_index;
        rep[3] = 0x3a;
        rep[5] = 0x01;
        rep[7] = 0x0e;
        for (i, &key) in chunk.iter().enumerate() {
            let off = 8 + i * 4;
            rep[off] = key;
            rep[off + 1] = r;
            rep[off + 2] = g;
            rep[off + 3] = b;
        }
        writer
            .write_output_report(&rep)
            .await
            .map_err(WriteError::Hid)?;
    }
    let mut commit = vec![0u8; 20];
    commit[0] = 0x11;
    commit[1] = device_index;
    commit[2] = feature_index;
    commit[3] = 0x5a;
    writer
        .write_output_report(&commit)
        .await
        .map_err(WriteError::Hid)?;
    debug!(
        device_index,
        feature_index, r, g, b, "wrote keyboard colour"
    );
    Ok(())
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
    let feature = open_feature::<AdjustableDpiFeature>(&mut device).await?;
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
    let SmartShiftStatus {
        mode,
        auto_disengage,
        tunable_torque,
    } = feature
        .get_status()
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    let next = mode.flipped();
    feature
        .set_status(next, auto_disengage, tunable_torque)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    debug!(index, ?next, "wrote SmartShift mode");
    Ok(next)
}

/// Write a full SmartShift configuration — wheel mode, auto-disengage
/// threshold, and tunable torque — to `route`. The firmware persists all three
/// to the device's NVM. Callers that mean to change only one field should read
/// the rest via [`get_smartshift_status`] first and pass them back unchanged.
///
/// `FeatureUnsupported` when the device doesn't expose HID++ `0x2111`.
pub async fn set_smartshift(
    route: &DeviceRoute,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_smartshift_on_channel(&channel, index, mode, auto_disengage, tunable_torque).await
    })
    .await
}

/// The SmartShift write itself, on an already-open channel at HID++ `index`.
/// Shared by [`set_smartshift`] and [`set_smartshift_on`].
async fn set_smartshift_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    mode: SmartShiftMode,
    auto_disengage: u8,
    tunable_torque: u8,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = open_feature::<SmartShiftFeatureV0>(&mut device).await?;
    feature
        .set_status(mode, auto_disengage, tunable_torque)
        .await
        .map_err(|e| WriteError::Hidpp(format!("{e:?}")))?;
    debug!(
        index,
        ?mode,
        auto_disengage,
        tunable_torque,
        "wrote SmartShift config"
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{DpiCapabilities, WriteError};

    #[test]
    fn capabilities_sort_and_deduplicate_values() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![1600, 400, 800, 800])?;

        assert_eq!(caps.values(), [400, 800, 1600]);
        assert_eq!(caps.min(), 400);
        assert_eq!(caps.max(), 1600);
        Ok(())
    }

    #[test]
    fn capabilities_reject_empty_list() {
        assert!(matches!(
            DpiCapabilities::new(Vec::new()),
            Err(WriteError::EmptyDpiList)
        ));
    }

    #[test]
    fn nearest_returns_closest_supported_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.nearest(390), 400);
        assert_eq!(caps.nearest(1000), 800);
        assert_eq!(caps.nearest(2000), 1600);
        Ok(())
    }

    #[test]
    fn step_hint_returns_smallest_positive_gap() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1200, 2000])?;

        assert_eq!(caps.step_hint(), 400);
        Ok(())
    }

    #[test]
    fn adjacent_test_target_prefers_next_then_previous_value() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.adjacent_test_target(400), Some(800));
        assert_eq!(caps.adjacent_test_target(800), Some(1600));
        assert_eq!(caps.adjacent_test_target(1600), Some(800));
        Ok(())
    }

    #[test]
    fn adjacent_test_target_handles_current_outside_list() -> Result<(), WriteError> {
        let caps = DpiCapabilities::new(vec![400, 800, 1600])?;

        assert_eq!(caps.adjacent_test_target(1000), Some(1600));
        assert_eq!(caps.adjacent_test_target(2000), Some(1600));
        Ok(())
    }
}
