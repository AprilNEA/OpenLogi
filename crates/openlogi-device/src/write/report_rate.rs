//! HID++ report-rate reads and writes (`0x8060` / `0x8061`).

use std::sync::Arc;

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        extended_report_rate::{
            ConnectionType, ExtendedReportRate, ExtendedReportRateFeature, ExtendedReportRateList,
        },
        report_rate::{ReportRateFeature, ReportRateList},
    },
    protocol::v20::{ErrorType, Hidpp20Error},
};

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::DeviceRoute;

use super::{HidppOperation, WriteError, classify_hidpp_error, with_route};

pub use openlogi_core::hid::report_rate::{ReportRateCapabilities, ReportRateHz, ReportRateInfo};

/// Whichever report-rate feature a device actually exposes.
enum ReportRateFeatureKind {
    /// `0x8060` — legacy millisecond intervals.
    Legacy(Arc<ReportRateFeature>),
    /// `0x8061` — gaming mice with discrete Hz values.
    Extended(Arc<ExtendedReportRateFeature>),
}

impl ReportRateFeatureKind {
    async fn open(device: &mut Device) -> Result<Self, WriteError> {
        if let Some(index) = feature_index(device, ExtendedReportRateFeature::ID).await? {
            return Ok(Self::Extended(device.add_feature(index)));
        }
        if let Some(index) = feature_index(device, ReportRateFeature::ID).await? {
            return Ok(Self::Legacy(device.add_feature(index)));
        }
        Err(WriteError::FeatureUnsupported {
            feature_hex: ReportRateFeature::ID,
        })
    }

    const fn id(&self) -> u16 {
        match self {
            Self::Legacy(_) => ReportRateFeature::ID,
            Self::Extended(_) => ExtendedReportRateFeature::ID,
        }
    }

    async fn supported_hz(&self) -> Result<Vec<u16>, Hidpp20Error> {
        match self {
            Self::Legacy(feature) => {
                let list = feature.get_report_rate_list().await?;
                Ok(legacy_list_to_hz(list))
            }
            Self::Extended(feature) => {
                let list = feature.get_actual_report_rate_list().await?;
                Ok(extended_list_to_hz(list))
            }
        }
    }

    async fn current_hz(&self, route: &DeviceRoute) -> Result<ReportRateHz, Hidpp20Error> {
        match self {
            Self::Legacy(feature) => {
                let ms = feature.get_report_rate().await?;
                ReportRateHz::from_interval_ms(ms).ok_or(Hidpp20Error::UnsupportedResponse)
            }
            Self::Extended(feature) => {
                let connection = connection_type_for_route(route);
                let rate = feature.get_report_rate(connection).await?;
                extended_rate_to_hz(rate)
            }
        }
    }

    async fn set_hz(&self, rate: ReportRateHz) -> Result<(), Hidpp20Error> {
        match self {
            Self::Legacy(feature) => {
                let ms = hz_to_interval_ms(rate)?;
                feature.set_report_rate(ms).await
            }
            Self::Extended(feature) => {
                let wire = hz_to_extended_rate(rate)?;
                feature.set_report_rate(wire).await
            }
        }
    }
}

async fn feature_index(device: &mut Device, feature_hex: u16) -> Result<Option<u8>, WriteError> {
    Ok(device
        .root()
        .get_feature(feature_hex)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::ResolveFeature, feature_hex))?
        .map(|info| info.index))
}

fn legacy_list_to_hz(list: ReportRateList) -> Vec<u16> {
    (1..=8u8)
        .filter_map(|ms| {
            let flag = ReportRateList::from_bits_retain(1 << (ms - 1));
            if !list.contains(flag) {
                return None;
            }
            ReportRateHz::from_interval_ms(ms).map(ReportRateHz::into_inner)
        })
        .collect()
}

fn extended_list_to_hz(list: ExtendedReportRateList) -> Vec<u16> {
    [
        (ExtendedReportRateList::HZ_125, 125),
        (ExtendedReportRateList::HZ_250, 250),
        (ExtendedReportRateList::HZ_500, 500),
        (ExtendedReportRateList::HZ_1000, 1000),
        (ExtendedReportRateList::HZ_2000, 2000),
        (ExtendedReportRateList::HZ_4000, 4000),
        (ExtendedReportRateList::HZ_8000, 8000),
    ]
    .into_iter()
    .filter_map(|(flag, hz)| list.contains(flag).then_some(hz))
    .collect()
}

fn extended_rate_to_hz(rate: ExtendedReportRate) -> Result<ReportRateHz, Hidpp20Error> {
    let hz = match rate {
        ExtendedReportRate::Hz125 => 125,
        ExtendedReportRate::Hz250 => 250,
        ExtendedReportRate::Hz500 => 500,
        ExtendedReportRate::Hz1000 => 1000,
        ExtendedReportRate::Hz2000 => 2000,
        ExtendedReportRate::Hz4000 => 4000,
        ExtendedReportRate::Hz8000 => 8000,
        _ => return Err(Hidpp20Error::UnsupportedResponse),
    };
    Ok(ReportRateHz::new(hz))
}

fn hz_to_extended_rate(rate: ReportRateHz) -> Result<ExtendedReportRate, Hidpp20Error> {
    match rate.into_inner() {
        125 => Ok(ExtendedReportRate::Hz125),
        250 => Ok(ExtendedReportRate::Hz250),
        500 => Ok(ExtendedReportRate::Hz500),
        1000 => Ok(ExtendedReportRate::Hz1000),
        2000 => Ok(ExtendedReportRate::Hz2000),
        4000 => Ok(ExtendedReportRate::Hz4000),
        8000 => Ok(ExtendedReportRate::Hz8000),
        _ => Err(Hidpp20Error::UnsupportedResponse),
    }
}

fn hz_to_interval_ms(rate: ReportRateHz) -> Result<u8, Hidpp20Error> {
    let hz = rate.into_inner();
    if hz == 0 || !1000u16.is_multiple_of(hz) {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    let ms = 1000 / hz;
    if !(1..=8).contains(&ms) {
        return Err(Hidpp20Error::UnsupportedResponse);
    }
    u8::try_from(ms).map_err(|_| Hidpp20Error::UnsupportedResponse)
}

fn connection_type_for_route(route: &DeviceRoute) -> ConnectionType {
    match route {
        DeviceRoute::Bolt { .. } | DeviceRoute::Unifying { .. } => ConnectionType::GamingWireless,
        DeviceRoute::Direct { .. } | DeviceRoute::RawHid { .. } => ConnectionType::Wired,
    }
}

fn classify_report_rate_error(feature_hex: u16, error: Hidpp20Error) -> WriteError {
    match error {
        Hidpp20Error::Feature(ErrorType::Unsupported | ErrorType::InvalidFunctionId)
        | Hidpp20Error::UnsupportedResponse => WriteError::FeatureUnsupported { feature_hex },
        other => classify_hidpp_error(other, HidppOperation::ReadReportRate, feature_hex),
    }
}

/// Read the current report rate and supported values for the device at `route`.
pub async fn get_report_rate_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ReportRateInfo, WriteError> {
    let index = route.device_index();
    let fetch_route = route.clone();
    with_route(backend, route, move |channel| async move {
        get_report_rate_info_on_channel(&channel, index, &fetch_route).await
    })
    .await
}

async fn get_report_rate_info_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
    route: &DeviceRoute,
) -> Result<ReportRateInfo, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = ReportRateFeatureKind::open(&mut device).await?;
    let feature_hex = feature.id();
    let current = feature
        .current_hz(route)
        .await
        .map_err(|e| classify_report_rate_error(feature_hex, e))?;
    let values = feature
        .supported_hz()
        .await
        .map_err(|e| classify_report_rate_error(feature_hex, e))?;
    Ok(ReportRateInfo {
        current,
        capabilities: ReportRateCapabilities::new(values)?,
    })
}

/// Set the report rate for the device at `route`.
pub async fn set_report_rate(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    rate: ReportRateHz,
) -> Result<(), WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_report_rate_on_channel(&channel, index, rate).await
    })
    .await
}

async fn set_report_rate_on_channel(
    channel: &Arc<hidpp::channel::HidppChannel>,
    index: u8,
    rate: ReportRateHz,
) -> Result<(), WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    let feature = ReportRateFeatureKind::open(&mut device).await?;
    let feature_hex = feature.id();
    feature
        .set_hz(rate)
        .await
        .map_err(|e| classify_hidpp_error(e, HidppOperation::WriteReportRate, feature_hex))
}

/// Write report rate on an already-open [`SharedChannel`].
pub async fn set_report_rate_on(
    shared: &SharedChannel,
    rate: ReportRateHz,
) -> Result<(), WriteError> {
    set_report_rate_on_channel(shared.channel(), shared.device_index(), rate).await
}

/// Read report rate on an already-open [`SharedChannel`].
pub async fn get_report_rate_info_on(
    shared: &SharedChannel,
    route: &DeviceRoute,
) -> Result<ReportRateInfo, WriteError> {
    get_report_rate_info_on_channel(shared.channel(), shared.device_index(), route).await
}
