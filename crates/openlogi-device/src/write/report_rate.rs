//! HID++ report-rate reads and writes — `0x8061 ExtendedAdjustableReportRate`
//! (gaming mice that report faster than 1 kHz), with the original `0x8060
//! AdjustableReportRate` as the fallback.
//!
//! Both are presented in whole hertz ([`ReportRate`]). `0x8060` speaks report
//! intervals of 1–8 ms, rounded to the nearest hertz (3 ms is 333 Hz).
//! `0x8061` names one of seven rates from 125 Hz to 8 kHz and keeps one per
//! connection type, wired or gaming wireless. Its `setReportRate` takes no
//! connection type — it sets the rate of the connection the device is on — so
//! the read that lists and checks the rate names that same connection,
//! inferred from the HID++ index the device answers at: the direct
//! self-index `0xff` is the USB cable, a receiver slot the wireless link.
//! The rates the device lists come from `getActualReportRateList`, which the
//! specification defines for the current connection.
//!
//! A write asks only for a rate the device lists, is skipped when the device
//! already reports at it, and is read back afterwards: a device whose onboard
//! profile owns the rate acknowledges the write and keeps its own, which
//! surfaces as [`WriteError::ReportRateNotApplied`] instead of a success
//! nothing observed.

use std::ops::RangeInclusive;
use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{
        CreatableFeature as _,
        extended_report_rate::{
            ConnectionType, ExtendedReportRate, ExtendedReportRateFeature, ExtendedReportRateList,
        },
        report_rate::{ReportRateFeature, ReportRateList},
    },
};
use tracing::debug;

use crate::SharedChannel;
use crate::backend::HidBackend;
use crate::channel::route::{DIRECT_DEVICE_INDEX, DeviceRoute};
use openlogi_core::hid::{ReportRate, ReportRateInfo};

use super::{HidppOperation, WriteError, classify_hidpp_error, open_feature, with_route};

/// The intervals `0x8060` can express, in milliseconds.
fn legacy_intervals_ms() -> RangeInclusive<u8> {
    1..=8
}

/// The rate of a `0x8060` report interval, rounded to the nearest hertz.
fn legacy_rate(interval_ms: u8) -> ReportRate {
    let interval_ms = u16::from(interval_ms.max(1));
    ReportRate::new((1000 + interval_ms / 2) / interval_ms)
}

/// The `0x8060` interval whose rate is `rate`, if there is one.
fn legacy_interval(rate: ReportRate) -> Option<u8> {
    legacy_intervals_ms().find(|&interval_ms| legacy_rate(interval_ms) == rate)
}

/// The rates in a `0x8060` interval list, ascending: bit `n` is an interval
/// of `n + 1` ms.
fn legacy_rates(list: ReportRateList) -> Vec<ReportRate> {
    legacy_intervals_ms()
        .rev()
        .filter(|interval_ms| list.bits() & (1 << (interval_ms - 1)) != 0)
        .map(legacy_rate)
        .collect()
}

/// The `0x8061` settings in wire order, slowest first.
const EXTENDED_RATES: [ExtendedReportRate; 7] = [
    ExtendedReportRate::Hz125,
    ExtendedReportRate::Hz250,
    ExtendedReportRate::Hz500,
    ExtendedReportRate::Hz1000,
    ExtendedReportRate::Hz2000,
    ExtendedReportRate::Hz4000,
    ExtendedReportRate::Hz8000,
];

/// The rate of a `0x8061` setting: 125 Hz doubled once per step.
fn extended_rate(setting: ExtendedReportRate) -> ReportRate {
    ReportRate::new(125 << u8::from(setting))
}

/// The `0x8061` setting whose rate is `rate`, if there is one.
fn extended_setting(rate: ReportRate) -> Option<ExtendedReportRate> {
    EXTENDED_RATES
        .into_iter()
        .find(|&setting| extended_rate(setting) == rate)
}

/// The rates in a `0x8061` rate list, ascending: bit `n` is the `n`th
/// setting. Bits past 8 kHz name no rate and are ignored.
fn extended_rates(list: ExtendedReportRateList) -> Vec<ReportRate> {
    EXTENDED_RATES
        .into_iter()
        .filter(|&setting| list.bits() & (1 << u8::from(setting)) != 0)
        .map(extended_rate)
        .collect()
}

/// The `0x8061` connection type of a device answering at HID++ `index`: the
/// direct self-index is the USB cable, a receiver slot the gaming-wireless
/// link.
fn connection_for(index: u8) -> ConnectionType {
    if index == DIRECT_DEVICE_INDEX {
        ConnectionType::Wired
    } else {
        ConnectionType::GamingWireless
    }
}

/// Whichever report-rate feature the device exposes, normalised onto hertz.
enum ReportRateControl {
    /// `0x8061`, read for the connection the device is on.
    Extended {
        feature: Arc<ExtendedReportRateFeature>,
        connection: ConnectionType,
    },
    /// `0x8060`.
    Legacy(Arc<ReportRateFeature>),
}

impl ReportRateControl {
    /// Open `0x8061`, or `0x8060` when the device has no `0x8061`. Any other
    /// failure to open `0x8061` propagates unchanged.
    async fn open(device: &mut Device, index: u8) -> Result<Self, WriteError> {
        match open_feature::<ExtendedReportRateFeature>(device).await {
            Ok(feature) => Ok(Self::Extended {
                feature,
                connection: connection_for(index),
            }),
            Err(WriteError::FeatureUnsupported { feature_hex })
                if feature_hex == ExtendedReportRateFeature::ID =>
            {
                Ok(Self::Legacy(
                    open_feature::<ReportRateFeature>(device).await?,
                ))
            }
            Err(error) => Err(error),
        }
    }

    fn feature_hex(&self) -> u16 {
        match self {
            Self::Extended { .. } => ExtendedReportRateFeature::ID,
            Self::Legacy(_) => ReportRateFeature::ID,
        }
    }

    /// The rate the device reports at now, failing as `operation`.
    async fn current(&self, operation: HidppOperation) -> Result<ReportRate, WriteError> {
        let classify = |e| classify_hidpp_error(e, operation, self.feature_hex());
        match self {
            Self::Extended {
                feature,
                connection,
            } => Ok(extended_rate(
                feature
                    .get_report_rate(*connection)
                    .await
                    .map_err(classify)?,
            )),
            Self::Legacy(feature) => Ok(legacy_rate(
                feature.get_report_rate().await.map_err(classify)?,
            )),
        }
    }

    /// The current rate and every rate the device lists, failing as
    /// `operation`. A device that lists none answered nothing usable.
    async fn info(&self, operation: HidppOperation) -> Result<ReportRateInfo, WriteError> {
        let classify = |e| classify_hidpp_error(e, operation, self.feature_hex());
        let supported = match self {
            Self::Extended { feature, .. } => extended_rates(
                feature
                    .get_actual_report_rate_list()
                    .await
                    .map_err(classify)?,
            ),
            Self::Legacy(feature) => {
                legacy_rates(feature.get_report_rate_list().await.map_err(classify)?)
            }
        };
        if supported.is_empty() {
            return Err(WriteError::UnsupportedResponse {
                operation,
                feature_hex: self.feature_hex(),
            });
        }
        let current = self.current(operation).await?;
        Ok(ReportRateInfo { current, supported })
    }

    /// Write `rate`, which the caller has checked the device lists.
    async fn write(&self, rate: ReportRate) -> Result<(), WriteError> {
        let unsupported = WriteError::ReportRateUnsupported { requested: rate };
        let classify =
            |e| classify_hidpp_error(e, HidppOperation::WriteReportRate, self.feature_hex());
        match self {
            Self::Extended { feature, .. } => feature
                .set_report_rate(extended_setting(rate).ok_or(unsupported)?)
                .await
                .map_err(classify),
            Self::Legacy(feature) => feature
                .set_report_rate(legacy_interval(rate).ok_or(unsupported)?)
                .await
                .map_err(classify),
        }
    }
}

async fn open_control(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<ReportRateControl, WriteError> {
    let mut device = Device::new(Arc::clone(channel), index)
        .await
        .map_err(|_| WriteError::DeviceUnreachable { index })?;
    ReportRateControl::open(&mut device, index).await
}

/// Read the report rate and the rates the device lists on `route`.
pub async fn get_report_rate_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<ReportRateInfo, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        get_report_rate_info_on_channel(&channel, index).await
    })
    .await
}

/// Read the report rate and the rates the device lists on an already-open
/// [`SharedChannel`].
pub async fn get_report_rate_info_on(shared: &SharedChannel) -> Result<ReportRateInfo, WriteError> {
    get_report_rate_info_on_channel(shared.channel(), shared.device_index()).await
}

async fn get_report_rate_info_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
) -> Result<ReportRateInfo, WriteError> {
    open_control(channel, index)
        .await?
        .info(HidppOperation::ReadReportRate)
        .await
}

/// Set the report rate on `route` and return what the device reports
/// afterwards. A rate the device does not list fails with
/// [`WriteError::ReportRateUnsupported`] before anything is written.
pub async fn set_report_rate(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    rate: ReportRate,
) -> Result<ReportRateInfo, WriteError> {
    let index = route.device_index();
    with_route(backend, route, move |channel| async move {
        set_report_rate_on_channel(&channel, index, rate).await
    })
    .await
}

/// Set the report rate on an already-open [`SharedChannel`] — the fast path
/// that skips enumeration and channel setup.
pub async fn set_report_rate_on(
    shared: &SharedChannel,
    rate: ReportRate,
) -> Result<ReportRateInfo, WriteError> {
    set_report_rate_on_channel(shared.channel(), shared.device_index(), rate).await
}

async fn set_report_rate_on_channel(
    channel: &Arc<HidppChannel>,
    index: u8,
    rate: ReportRate,
) -> Result<ReportRateInfo, WriteError> {
    let control = open_control(channel, index).await?;
    let before = control.info(HidppOperation::WriteReportRate).await?;
    if !before.supports(rate) {
        return Err(WriteError::ReportRateUnsupported { requested: rate });
    }
    if before.current == rate {
        debug!(index, %rate, "report rate already set");
        return Ok(before);
    }
    control.write(rate).await?;
    match control.current(HidppOperation::WriteReportRate).await {
        Ok(actual) if actual == rate => {
            debug!(index, %rate, "report rate written");
            Ok(ReportRateInfo {
                current: actual,
                ..before
            })
        }
        Ok(actual) => Err(WriteError::ReportRateNotApplied {
            requested: rate,
            actual,
        }),
        Err(error) => {
            // The write itself was acknowledged. A rate change can make a
            // wireless link resynchronise, so a read straight after it may
            // time out; the DPI write treats its read-back the same way.
            debug!(index, %rate, %error, "report rate written (read-back skipped)");
            Ok(ReportRateInfo {
                current: rate,
                ..before
            })
        }
    }
}

#[cfg(test)]
mod tests;
