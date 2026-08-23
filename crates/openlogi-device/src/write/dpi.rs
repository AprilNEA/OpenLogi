use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        adjustable_dpi::AdjustableDpiFeature,
        extended_dpi::{DpiDirection, DpiRange, ExtendedDpiFeature, SetDpiParameters},
    },
    protocol::v20::{ErrorType, Hidpp20Error},
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, with_route};

// DpiCapabilities and DpiInfo are pure IPC wire data with no HID++ I/O, so
// they live in `openlogi_core::hid::dpi`; re-exported here unchanged so this
// module's own API surface doesn't churn.
pub use openlogi_core::hid::dpi::{Dpi, DpiCapabilities, DpiInfo};

/// Sensor 0 is the only sensor OpenLogi drives: the UI exposes one DPI value
/// per device, and every Logitech pointing device reports its pointer sensor
/// first.
const SENSOR: u8 = 0;

/// Whichever DPI feature a device actually exposes.
///
/// `0x2201 AdjustableDpi` is the original; `0x2202 ExtendedAdjustableDpi` is
/// its successor, and some mice expose only the latter (`openlogi diag
/// features` shows which). `Capabilities::from_feature_ids` turns the DPI panel
/// on for *either* ID, so both have to be drivable from here — otherwise a
/// `0x2202`-only mouse gets a panel that cannot read or write anything.
enum DpiFeature {
    /// `0x2201` — one DPI per sensor, described as a flat list of values.
    Adjustable(Arc<AdjustableDpiFeature>),

    /// `0x2202` — independent X/Y DPI plus lift-off distance, described as a
    /// mix of fixed values and stepped ranges.
    Extended(Arc<ExtendedDpiFeature>),
}

impl DpiFeature {
    /// Opens whichever DPI feature `device` exposes, preferring `0x2201`.
    ///
    /// The preference is deliberate and not protocol-driven: `0x2201` is the
    /// path every device that works today already takes, so trying it first
    /// keeps `0x2202` support purely additive. A device exposing both behaves
    /// exactly as it did before.
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        if let Some(index) = feature_index(device, AdjustableDpiFeature::ID).await? {
            return Ok(Self::Adjustable(device.add_feature(index)));
        }
        if let Some(index) = feature_index(device, ExtendedDpiFeature::ID).await? {
            return Ok(Self::Extended(device.add_feature(index)));
        }
        // Neither ID is present. Name the canonical one in the error: a caller
        // reading "0x2201 unsupported" is being told this device has no DPI
        // feature at all, which is what happened.
        Err(WriteError::FeatureUnsupported {
            feature_hex: AdjustableDpiFeature::ID,
        })
    }

    /// The HID++ feature ID being driven, for error reporting.
    const fn id(&self) -> u16 {
        match self {
            Self::Adjustable(_) => AdjustableDpiFeature::ID,
            Self::Extended(_) => ExtendedDpiFeature::ID,
        }
    }

    /// The number of motion sensors the device reports.
    async fn sensor_count(&self) -> Result<u8, Hidpp20Error> {
        match self {
            Self::Adjustable(feature) => feature.get_sensor_count().await,
            Self::Extended(feature) => feature.get_sensor_count().await,
        }
    }

    /// The DPI currently configured on [`SENSOR`].
    async fn current_dpi(&self) -> Result<Dpi, Hidpp20Error> {
        match self {
            Self::Adjustable(feature) => feature.get_sensor_dpi(SENSOR).await.map(Dpi::from),
            Self::Extended(feature) => Ok(feature
                .get_sensor_dpi_parameters(SENSOR)
                .await?
                .dpi_x
                .into()),
        }
    }

    /// Every DPI value [`SENSOR`] accepts, as a flat list.
    async fn supported_dpi(&self) -> Result<Vec<u16>, Hidpp20Error> {
        match self {
            Self::Adjustable(feature) => feature.get_sensor_dpi_list(SENSOR).await,
            Self::Extended(feature) => {
                // `getSensorDpiList` (function 3) only answers on sensors that
                // support profiles; the range description is the one every
                // 0x2202 sensor reports. X is the axis the UI drives.
                let ranges = feature
                    .get_sensor_dpi_ranges(SENSOR, DpiDirection::X)
                    .await?;
                Ok(expand_dpi_ranges(&ranges))
            }
        }
    }

    /// Sets [`SENSOR`]'s DPI.
    async fn set_dpi(&self, dpi: Dpi) -> Result<(), Hidpp20Error> {
        let dpi = dpi.into();
        match self {
            Self::Adjustable(feature) => feature.set_sensor_dpi(SENSOR, dpi).await,
            Self::Extended(feature) => {
                // `setSensorDpiParameters` writes DPI X, DPI Y and lift-off
                // distance in one packet with no "leave unchanged" encoding, so
                // read the current parameters first and put back what we are
                // not asked to change. Writing a bare `lod` would silently
                // retune the sensor's lift-off height.
                let current = feature.get_sensor_dpi_parameters(SENSOR).await?;
                feature
                    .set_sensor_dpi_parameters(
                        SENSOR,
                        SetDpiParameters {
                            dpi_x: dpi,
                            // The spec has the host send 0 for dpiY when the
                            // sensor has no independent Y axis, and reports 0
                            // on read in exactly that case. When it does have
                            // one, keep the axes locked together — the UI
                            // exposes a single DPI.
                            dpi_y: if current.dpi_y == 0 { 0 } else { dpi },
                            lod: current.lod,
                        },
                    )
                    .await
            }
        }
    }
}

/// Resolves `feature_hex` to its runtime index, or `None` when the device does
/// not expose it.
///
/// Unlike [`open_feature`](super::open_feature) an absent feature is not an
/// error here — [`DpiFeature::open`] uses absence to fall through to the next
/// candidate, and only a transport failure should abort the probe.
async fn feature_index(device: &mut Device, feature_hex: u16) -> Result<Option<u8>, WriteError> {
    Ok(device
        .root()
        .get_feature(feature_hex)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, feature_hex))?
        .map(|info| info.index))
}

/// Flattens `0x2202`'s fixed-value / stepped-range description into the flat
/// list [`DpiCapabilities`] is built from.
///
/// A stepped range's endpoints are inclusive and the high endpoint is always
/// selectable even when it is not an exact multiple of `step` from the low one.
/// Adjacent ranges may share an endpoint; `DpiCapabilities::new` deduplicates.
pub(super) fn expand_dpi_ranges(ranges: &[DpiRange]) -> Vec<u16> {
    let mut values = Vec::new();
    for range in ranges {
        match *range {
            DpiRange::Fixed(value) => values.push(value),
            DpiRange::Stepped { from, to, step } => {
                // `step` is never 0 and `to >= from` — the decoder rejects both
                // as a malformed response — so this terminates.
                let mut value = u32::from(from);
                while value < u32::from(to) {
                    if let Ok(value) = u16::try_from(value) {
                        values.push(value);
                    }
                    value += u32::from(step);
                }
                values.push(to);
            }
        }
    }
    values
}

/// Read the device's current DPI on sensor 0 — companion to [`set_dpi`].
/// Used by `openlogi diag dpi` and any future Settings → Diagnostics
/// surface that wants to display the current value without writing.
pub async fn get_dpi(backend: &dyn HidBackend, route: &DeviceRoute) -> Result<Dpi, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_dpi_on_channel(&channel, index).await
    })
    .await
}

async fn get_dpi_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
) -> Result<Dpi, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = DpiFeature::open(&mut device).await?;
    feature
        .current_dpi()
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ReadDpi, feature.id()))
}

/// Classify a HID++ error from the DPI functions of `feature_hex`. A device
/// that announces the feature but rejects a function (`Unsupported` /
/// `InvalidFunctionId`) or returns a structurally invalid DPI description
/// (`UnsupportedResponse`) will keep doing so, so these map to the permanent
/// [`WriteError::FeatureUnsupported`]; channel/timeout and other errors are
/// forwarded through [`classify_hidpp_error`] as transient so callers may retry.
fn classify_dpi_error(feature_hex: u16, error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Feature(ErrorType::Unsupported | ErrorType::InvalidFunctionId)
        | Hidpp20Error::UnsupportedResponse => WriteError::FeatureUnsupported { feature_hex },
        other => classify_hidpp_error(other, HidppOperation::ReadDpiCapabilities, feature_hex),
    }
}

/// Read the current DPI and the supported DPI values for sensor 0 in one
/// route/channel session.
pub async fn get_dpi_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<DpiInfo, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_dpi_info_on_channel(&channel, index).await
    })
    .await
}

pub(super) async fn get_dpi_info_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
) -> Result<DpiInfo, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = DpiFeature::open(&mut device).await?;
    let feature_hex = feature.id();
    let sensor_count = feature
        .sensor_count()
        .await
        .map_err(|e| classify_dpi_error(feature_hex, e))?;
    if sensor_count == 0 {
        // The device claims a DPI feature but exposes no sensor — it cannot
        // report DPI, and that won't change on retry.
        return Err(WriteError::FeatureUnsupported { feature_hex });
    }
    let current = feature
        .current_dpi()
        .await
        .map_err(|e| classify_dpi_error(feature_hex, e))?;
    let values = feature
        .supported_dpi()
        .await
        .map_err(|e| classify_dpi_error(feature_hex, e))?;
    Ok(DpiInfo {
        current,
        capabilities: DpiCapabilities::new(values)?,
    })
}

/// Set sensor 0's DPI for the device addressed by `route`.
pub async fn set_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    dpi: Dpi,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_dpi_on_channel(&channel, index, dpi).await
    })
    .await
}

/// The DPI write itself, on an already-open channel at HID++ `index`. Shared by
/// [`set_dpi`] (which opens a fresh channel) and [`set_dpi_on`]
/// (which reuses one).
pub(super) async fn set_dpi_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
    dpi: Dpi,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = DpiFeature::open(&mut device).await?;
    feature
        .set_dpi(dpi)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteDpi, feature.id()))?;
    // Read back to confirm the firmware accepted the value. A mismatch is a
    // silent failure mode that's otherwise invisible — devices in low-power
    // states or with unsupported DPI ranges can ACK the write yet keep the old
    // value. We log a warning but still return Ok because the request reached
    // the device.
    if let Ok(actual) = feature.current_dpi().await {
        if actual == dpi {
            debug!(index, %dpi, "wrote DPI (verified)");
        } else {
            tracing::warn!(
                index,
                requested = %dpi,
                %actual,
                "DPI write accepted but device reports a different value — \
                 likely out of the device's supported range"
            );
        }
    } else {
        debug!(index, %dpi, "wrote DPI (read-back skipped)");
    }
    Ok(())
}

/// Write DPI on an already-open [`SharedChannel`] — the fast path that skips
/// enumeration and channel setup.
pub async fn set_dpi_on(shared: &SharedChannel, dpi: Dpi) -> Result<(), WriteError> {
    set_dpi_on_channel(shared.channel(), shared.device_index(), dpi).await
}

/// Read current DPI and supported values on an already-open [`SharedChannel`].
pub async fn get_dpi_info_on(shared: &SharedChannel) -> Result<DpiInfo, WriteError> {
    get_dpi_info_on_channel(shared.channel(), shared.device_index()).await
}
