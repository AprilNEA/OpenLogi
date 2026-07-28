use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{CreatableFeature, adjustable_dpi::AdjustableDpiFeature},
    protocol::v20::{ErrorType, Hidpp20Error},
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// Supported DPI values reported by a device's HID++ AdjustableDpi feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            .array_windows::<2>()
            .filter_map(|&[low, high]| high.checked_sub(low))
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
///
/// Crosses the agent↔GUI IPC (`read_dpi`, [`DpiCapabilities`] included), so
/// field order is wire format — changes require a `PROTOCOL_VERSION` bump
/// (guarded by `openlogi-agent-core/tests/wire_format.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpiInfo {
    /// DPI currently configured on sensor 0.
    pub current: u16,
    /// Supported values reported by the device for sensor 0.
    pub capabilities: DpiCapabilities,
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
            .map_err(|e| classify_hidpp_error(e, HidppOperation::ReadDpi, AdjustableDpiFeature::ID))
    })
    .await
}

/// Classify a HID++ error from the AdjustableDpi functions. A device that
/// announces `0x2201` but rejects a function (`Unsupported` /
/// `InvalidFunctionId`) or returns a structurally invalid DPI list
/// (`UnsupportedResponse`) will keep doing so, so these map to the permanent
/// [`WriteError::FeatureUnsupported`]; channel/timeout and other errors are
/// forwarded through [`classify_hidpp_error`] as transient so callers may retry.
fn classify_dpi_error(error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Feature(ErrorType::Unsupported | ErrorType::InvalidFunctionId)
        | Hidpp20Error::UnsupportedResponse => WriteError::FeatureUnsupported {
            feature_hex: AdjustableDpiFeature::ID,
        },
        other => classify_hidpp_error(
            other,
            HidppOperation::ReadDpiCapabilities,
            AdjustableDpiFeature::ID,
        ),
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

/// Set sensor 0's DPI for the device addressed by `route`.
pub async fn set_dpi(route: &DeviceRoute, dpi: u16) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(route, move |channel| async move {
        set_dpi_on_channel(&channel, index, dpi).await
    })
    .await
}

/// The DPI write itself, on an already-open channel at HID++ `index`. Shared by
/// [`set_dpi`] (which opens a fresh channel) and [`set_dpi_on`](super::set_dpi_on)
/// (which reuses one).
pub(super) async fn set_dpi_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
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
        .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteDpi, AdjustableDpiFeature::ID))?;
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
