use std::{
    sync::{Arc, LazyLock, Mutex, PoisonError, Weak},
    time::Duration,
};

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
use crate::channel::DeviceCacheIdentity;
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

/// Brief pause before retrying a valid DPI write that the firmware rejected as
/// transiently busy/internal. The PRO X SUPERLIGHT 2 DEX has been observed to
/// return either response while processing closely spaced host writes, then
/// accept the identical request immediately afterward.
const TRANSIENT_WRITE_RETRY_DELAY: Duration = Duration::from_millis(50);

/// One initial write plus this many retries keeps the live control responsive
/// without turning a genuinely wedged device into a long-running IPC call.
const TRANSIENT_WRITE_RETRIES: u8 = 2;

/// Whichever DPI feature a device actually exposes.
///
/// `0x2201 AdjustableDpi` is the original; `0x2202 ExtendedAdjustableDpi` is
/// its successor, and some mice expose only the latter (`openlogi diag
/// features` shows which). `Capabilities::from_feature_ids` turns the DPI panel
/// on for *either* ID, so both have to be drivable from here — otherwise a
/// `0x2202`-only mouse gets a panel that cannot read or write anything.
enum DpiFeature {
    /// `0x2201` — one DPI per sensor, described as a flat list of values.
    Adjustable {
        feature: Arc<AdjustableDpiFeature>,
        index: u8,
    },

    /// `0x2202` — independent X/Y DPI plus lift-off distance, described as a
    /// mix of fixed values and stepped ranges.
    Extended {
        feature: Arc<ExtendedDpiFeature>,
        index: u8,
    },
}

#[derive(Clone, Copy)]
enum DpiFeatureAddress {
    Adjustable(u8),
    Extended(u8),
}

struct DpiCacheEntry {
    channel: Weak<hidpp::channel::HidppChannel>,
    identity: DeviceCacheIdentity,
    device_index: u8,
    feature: DpiFeatureAddress,
    capabilities: Option<DpiCapabilities>,
}

/// DPI feature addresses and ranges are immutable for one physical device.
/// Keep them beside a weak channel and physical identity so repeated UI
/// reads skip the root handshake without pinning a retired receiver or letting
/// a newly paired mouse inherit the former occupant's metadata.
static DPI_CACHE: LazyLock<Mutex<Vec<DpiCacheEntry>>> = LazyLock::new(|| Mutex::new(Vec::new()));

impl DpiFeature {
    /// Opens whichever DPI feature `device` exposes, preferring `0x2201`.
    ///
    /// The preference is deliberate and not protocol-driven: `0x2201` is the
    /// path every device that works today already takes, so trying it first
    /// keeps `0x2202` support purely additive. A device exposing both behaves
    /// exactly as it did before.
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        if let Some(index) = feature_index(device, AdjustableDpiFeature::ID).await? {
            return Ok(Self::Adjustable {
                feature: device.add_feature(index),
                index,
            });
        }
        if let Some(index) = feature_index(device, ExtendedDpiFeature::ID).await? {
            return Ok(Self::Extended {
                feature: device.add_feature(index),
                index,
            });
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
            Self::Adjustable { .. } => AdjustableDpiFeature::ID,
            Self::Extended { .. } => ExtendedDpiFeature::ID,
        }
    }

    const fn address(&self) -> DpiFeatureAddress {
        match self {
            Self::Adjustable { index, .. } => DpiFeatureAddress::Adjustable(*index),
            Self::Extended { index, .. } => DpiFeatureAddress::Extended(*index),
        }
    }

    /// The number of motion sensors the device reports.
    async fn sensor_count(&self) -> Result<u8, Hidpp20Error> {
        match self {
            Self::Adjustable { feature, .. } => feature.get_sensor_count().await,
            Self::Extended { feature, .. } => feature.get_sensor_count().await,
        }
    }

    /// The DPI currently configured on [`SENSOR`].
    async fn current_dpi(&self) -> Result<Dpi, Hidpp20Error> {
        match self {
            Self::Adjustable { feature, .. } => feature.get_sensor_dpi(SENSOR).await.map(Dpi::from),
            Self::Extended { feature, .. } => Ok(feature
                .get_sensor_dpi_parameters(SENSOR)
                .await?
                .dpi_x
                .into()),
        }
    }

    /// Every DPI value [`SENSOR`] accepts, as a flat list.
    async fn supported_dpi(&self) -> Result<Vec<u16>, Hidpp20Error> {
        match self {
            Self::Adjustable { feature, .. } => feature.get_sensor_dpi_list(SENSOR).await,
            Self::Extended { feature, .. } => {
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
    async fn set_dpi(&self, dpi: Dpi) -> Result<bool, Hidpp20Error> {
        let dpi = dpi.into();
        match self {
            Self::Adjustable { feature, .. } => {
                if feature.get_sensor_dpi(SENSOR).await? == dpi {
                    return Ok(false);
                }
                feature.set_sensor_dpi(SENSOR, dpi).await?;
                Ok(true)
            }
            Self::Extended { feature, .. } => {
                // `setSensorDpiParameters` writes DPI X, DPI Y and lift-off
                // distance in one packet with no "leave unchanged" encoding, so
                // read the current parameters first and put back what we are
                // not asked to change. Writing a bare `lod` would silently
                // retune the sensor's lift-off height.
                let current = feature.get_sensor_dpi_parameters(SENSOR).await?;
                if current.dpi_x == dpi && (current.dpi_y == 0 || current.dpi_y == dpi) {
                    return Ok(false);
                }
                let params = SetDpiParameters {
                    dpi_x: dpi,
                    // The spec has the host send 0 for dpiY when the sensor
                    // has no independent Y axis, and reports 0 on read in
                    // exactly that case. When it does have one, keep the axes
                    // locked together — the UI exposes a single DPI.
                    dpi_y: if current.dpi_y == 0 { 0 } else { dpi },
                    lod: current.lod,
                };
                let mut retries = 0;
                loop {
                    let result = feature.set_sensor_dpi_parameters(SENSOR, params).await;
                    let transient = matches!(
                        &result,
                        Err(Hidpp20Error::Feature(
                            ErrorType::Busy | ErrorType::LogitechInternal
                        ))
                    );
                    if !transient || retries >= TRANSIENT_WRITE_RETRIES {
                        result?;
                        return Ok(true);
                    }
                    retries += 1;
                    debug!(retries, %dpi, "retrying transient extended-DPI write");
                    tokio::time::sleep(TRANSIENT_WRITE_RETRY_DELAY).await;
                }
            }
        }
    }
}

impl DpiFeatureAddress {
    fn bind(self, channel: &Arc<hidpp::channel::HidppChannel>, device_index: u8) -> DpiFeature {
        match self {
            Self::Adjustable(index) => DpiFeature::Adjustable {
                feature: Arc::new(AdjustableDpiFeature::new(
                    Arc::clone(channel),
                    device_index,
                    index,
                )),
                index,
            },
            Self::Extended(index) => DpiFeature::Extended {
                feature: Arc::new(ExtendedDpiFeature::new(
                    Arc::clone(channel),
                    device_index,
                    index,
                )),
                index,
            },
        }
    }
}

fn cached_dpi(
    channel: &Arc<hidpp::channel::HidppChannel>,
    identity: Option<&DeviceCacheIdentity>,
    device_index: u8,
) -> Option<(DpiFeatureAddress, Option<DpiCapabilities>)> {
    let identity = identity?;
    let mut cache = DPI_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|entry| entry.channel.strong_count() > 0);
    cache
        .iter()
        .find(|entry| {
            entry.device_index == device_index
                && entry.identity == *identity
                && entry
                    .channel
                    .upgrade()
                    .is_some_and(|cached| Arc::ptr_eq(&cached, channel))
        })
        .map(|entry| (entry.feature, entry.capabilities.clone()))
}

fn cache_dpi_feature(
    channel: &Arc<hidpp::channel::HidppChannel>,
    identity: Option<&DeviceCacheIdentity>,
    device_index: u8,
    feature: DpiFeatureAddress,
) {
    let Some(identity) = identity else {
        return;
    };
    let mut cache = DPI_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|entry| entry.channel.strong_count() > 0);
    if let Some(entry) = cache.iter_mut().find(|entry| {
        entry.device_index == device_index
            && entry.identity == *identity
            && entry
                .channel
                .upgrade()
                .is_some_and(|cached| Arc::ptr_eq(&cached, channel))
    }) {
        entry.feature = feature;
        return;
    }
    cache.push(DpiCacheEntry {
        channel: Arc::downgrade(channel),
        identity: identity.clone(),
        device_index,
        feature,
        capabilities: None,
    });
}

fn cache_dpi_capabilities(
    channel: &Arc<hidpp::channel::HidppChannel>,
    identity: Option<&DeviceCacheIdentity>,
    device_index: u8,
    capabilities: DpiCapabilities,
) {
    let Some(identity) = identity else {
        return;
    };
    let mut cache = DPI_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = cache.iter_mut().find(|entry| {
        entry.device_index == device_index
            && entry.identity == *identity
            && entry
                .channel
                .upgrade()
                .is_some_and(|cached| Arc::ptr_eq(&cached, channel))
    }) {
        entry.capabilities = Some(capabilities);
    }
}

async fn open_dpi_feature(
    channel: &Arc<hidpp::channel::HidppChannel>,
    identity: Option<&DeviceCacheIdentity>,
    device_index: u8,
) -> Result<(DpiFeature, Option<DpiCapabilities>), WriteError> {
    if let Some((address, capabilities)) = cached_dpi(channel, identity, device_index) {
        return Ok((address.bind(channel, device_index), capabilities));
    }
    let mut device = Device::new(Arc::clone(channel), device_index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable {
            index: device_index,
        })?;
    let feature = DpiFeature::open(&mut device).await?;
    cache_dpi_feature(channel, identity, device_index, feature.address());
    Ok((feature, None))
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
    let (feature, _) = open_dpi_feature(channel, None, index).await?;
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
    get_dpi_info_on_identified_channel(channel, None, index).await
}

async fn get_dpi_info_on_identified_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    identity: Option<&DeviceCacheIdentity>,
    index: u8,
) -> Result<DpiInfo, WriteError> {
    let (feature, cached_capabilities) = open_dpi_feature(channel, identity, index).await?;
    let feature_hex = feature.id();
    if let Some(capabilities) = cached_capabilities {
        let current = feature
            .current_dpi()
            .await
            .map_err(|e| classify_dpi_error(feature_hex, e))?;
        return Ok(DpiInfo {
            current,
            capabilities,
        });
    }
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
    let capabilities = DpiCapabilities::new(values)?;
    cache_dpi_capabilities(channel, identity, index, capabilities.clone());
    Ok(DpiInfo {
        current,
        capabilities,
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
    set_dpi_on_identified_channel(channel, None, index, dpi).await
}

async fn set_dpi_on_identified_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    identity: Option<&DeviceCacheIdentity>,
    index: u8,
    dpi: Dpi,
) -> Result<(), WriteError> {
    let (feature, _) = open_dpi_feature(channel, identity, index).await?;
    let wrote = feature
        .set_dpi(dpi)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteDpi, feature.id()))?;
    if !wrote {
        debug!(index, %dpi, "DPI already matches; write skipped");
        return Ok(());
    }
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
    set_dpi_on_identified_channel(
        shared.channel(),
        shared.cache_identity(),
        shared.device_index(),
        dpi,
    )
    .await
}

/// Read current DPI and supported values on an already-open [`SharedChannel`].
pub async fn get_dpi_info_on(shared: &SharedChannel) -> Result<DpiInfo, WriteError> {
    get_dpi_info_on_identified_channel(
        shared.channel(),
        shared.cache_identity(),
        shared.device_index(),
    )
    .await
}
