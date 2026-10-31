//! The G700's live pointer settings in the terms the rest of OpenLogi uses:
//! the DPI and the report rate, as [`DpiInfo`] and [`ReportRateInfo`].
//! The generic DPI and report-rate entry points route a G700 here.
//!
//! # What OpenLogi changes, and what the mouse keeps
//!
//! The mouse is the source of truth. Its active onboard profile holds five
//! DPI modes, the mode it starts in, and a report interval, in flash; the
//! DPI it runs at (register `0x63`) and the interval it reports at (`0x64`)
//! are live copies the mouse itself rewrites whenever its DPI buttons cycle
//! the modes or it selects a profile — at power-up among others.
//!
//! These entry points read and write the live registers only. A DPI or rate
//! chosen in OpenLogi lasts until the mouse next switches mode or profile;
//! after that the mouse plays its profile again, and the next read shows it.
//! What the mouse should start with is a profile edit
//! ([`super::set_g700_profile_dpi`], [`super::set_g700_profile_report_interval`]):
//! a flash page rewrite of a few seconds that briefly plays the factory
//! profile, which is not what a slider release or a rate pick should cost.
//!
//! This is also why nothing re-applies these settings when the mouse
//! reconnects. The agent's reconnect reapply writes the DPI and rate saved
//! for a device; the settings app saves them only for a device with a
//! persistent identity, which the G700 lacks (its receiver refuses the
//! pairing register, and it has no `DeviceInformation` feature), so there is
//! nothing saved to write over what the mouse plays.

use std::sync::LazyLock;

use hidpp::register::{
    current_profile::{CurrentProfile, read_current_profile},
    onboard_memory::{
        directory::DIRECTORY_PAGE,
        g700::{
            DPI_MODE_COUNT, DPI_RAW_MAX, G700Profile, dpi_from_raw, parse_directory, raw_from_dpi,
        },
        read_page,
    },
    report_rate::{ReportInterval, read_report_interval},
    resolution::{Resolution, read_current_resolution},
};
use openlogi_core::hid::{Dpi, DpiCapabilities, DpiInfo, ReportRate, ReportRateInfo};
use tracing::debug;

use super::G700Link;
use super::configure::{
    G700Error, profile_interval_valid, set_report_interval_on, set_resolution_on,
};
use crate::write::{DpiTarget, HidppOperation, WriteError};

/// Every DPI the sensor table names, ascending: 25 to 5700 in steps of 25.
static SUPPORTED_DPI: LazyLock<Vec<u16>> = LazyLock::new(|| {
    let mut values: Vec<u16> = (1..=DPI_RAW_MAX)
        .filter_map(dpi_from_raw)
        .map(std::num::NonZeroU16::get)
        .collect();
    values.dedup();
    values
});

/// The DPI values the G700 can run at.
fn dpi_capabilities() -> Result<DpiCapabilities, WriteError> {
    DpiCapabilities::new(SUPPORTED_DPI.clone())
}

/// The rates the G700 offers, ascending.
fn supported_rates() -> Vec<ReportRate> {
    [8, 4, 2, 1]
        .into_iter()
        .filter_map(|millis| ReportInterval::try_from(millis).ok())
        .map(rate_of)
        .collect()
}

fn rate_of(interval: ReportInterval) -> ReportRate {
    ReportRate::new(interval.rate_hz())
}

/// The interval whose rate is `rate`, when it is one a profile stores.
fn interval_of(rate: ReportRate) -> Option<ReportInterval> {
    let millis = u8::try_from(1000 / u16::from(rate).max(1)).ok()?;
    ReportInterval::try_from(millis)
        .ok()
        .filter(|interval| profile_interval_valid(*interval) && rate_of(*interval) == rate)
}

/// The DPI of the X step of `resolution`, the axis OpenLogi drives.
fn dpi_of(resolution: Resolution, operation: HidppOperation) -> Result<Dpi, WriteError> {
    u8::try_from(resolution.x)
        .ok()
        .and_then(dpi_from_raw)
        .map(|dpi| Dpi::new(dpi.get()))
        .ok_or_else(|| {
            WriteError::Hidpp(format!(
                "{operation:?}: raw resolution step {} has no DPI table entry",
                resolution.x
            ))
        })
}

/// The live DPI and every DPI the sensor table names.
pub(crate) async fn dpi_info(link: &G700Link) -> Result<DpiInfo, WriteError> {
    let operation = HidppOperation::ReadDpiCapabilities;
    let resolution = read_current_resolution(link.chan(), link.device_index())
        .await
        .map_err(|source| register_error(source, operation))?;
    Ok(DpiInfo {
        current: dpi_of(resolution, operation)?,
        capabilities: dpi_capabilities()?,
    })
}

/// Make the sensor run at `target`, both axes, until the mouse next switches
/// DPI mode or profile, and return the DPI written — `None` when
/// [`DpiTarget::Default`] finds no default.
pub(crate) async fn set_dpi_target(
    link: &G700Link,
    target: DpiTarget,
) -> Result<Option<Dpi>, WriteError> {
    let supported = dpi_capabilities()?;
    let dpi = match target {
        DpiTarget::Value(dpi) => dpi,
        DpiTarget::Lowest => supported.min(),
        DpiTarget::Default => {
            let Some(default) = profile_default_dpi(link).await? else {
                debug!("the G700 plays no profile DPI mode to default to — nothing written");
                return Ok(None);
            };
            supported.nearest(default)
        }
    };
    if !supported.contains(dpi) {
        return Err(WriteError::DpiUnsupported { requested: dpi });
    }
    let raw = raw_from_dpi(dpi.into_inner());
    let requested = Resolution {
        x: raw.into(),
        y: raw.into(),
    };
    // One re-write, as the HID++ 2.0 path does: a mouse waking from sleep
    // can acknowledge a write it has not applied.
    let mut attempt = 0;
    loop {
        attempt += 1;
        match set_resolution_on(link.chan(), link.device_index(), requested).await {
            Ok(_) => return Ok(Some(dpi)),
            Err(G700Error::ResolutionNotApplied { actual, .. }) if attempt == 1 => {
                debug!(?actual, %dpi, "G700 kept another resolution — writing once more");
            }
            Err(G700Error::ResolutionNotApplied { actual, .. }) => {
                return Err(WriteError::DpiNotApplied {
                    requested: dpi,
                    actual: dpi_of(actual, HidppOperation::WriteDpi)?,
                });
            }
            // The write was acknowledged; only its read-back failed.
            Err(G700Error::RegisterRead { source, .. }) => {
                debug!(?source, %dpi, "wrote the G700 resolution (read-back failed)");
                return Ok(Some(dpi));
            }
            Err(error) => return Err(g700_error(error, HidppOperation::WriteDpi)),
        }
    }
}

/// The DPI of the mode the played profile starts in, or `None` when the
/// mouse plays its factory profile or the profile names no valid mode.
async fn profile_default_dpi(link: &G700Link) -> Result<Option<Dpi>, WriteError> {
    let operation = HidppOperation::ReadDpi;
    let (chan, index) = (link.chan(), link.device_index());
    let current = read_current_profile(chan, index)
        .await
        .map_err(|source| register_error(source, operation))?;
    let page = match current {
        CurrentProfile::Factory => return Ok(None),
        CurrentProfile::Address { page, .. } => page,
        CurrentProfile::Index(position) => {
            let directory = read_page(chan, index, DIRECTORY_PAGE)
                .await
                .map_err(|source| register_error(source, operation))?;
            let Some(entry) = parse_directory(&directory)
                .entries()
                .get(usize::from(position))
                .copied()
            else {
                return Ok(None);
            };
            entry.page
        }
    };
    let profile = G700Profile::parse(
        &read_page(chan, index, page)
            .await
            .map_err(|source| register_error(source, operation))?,
    );
    let mode = usize::from(profile.default_dpi_mode);
    if mode >= DPI_MODE_COUNT {
        return Ok(None);
    }
    Ok(profile.dpi_modes[mode]
        .x_dpi()
        .map(|dpi| Dpi::new(dpi.get())))
}

/// The live report rate and the rates the G700 offers.
pub(crate) async fn report_rate_info(link: &G700Link) -> Result<ReportRateInfo, WriteError> {
    let interval = read_report_interval(link.chan(), link.device_index())
        .await
        .map_err(|source| register_error(source, HidppOperation::ReadReportRate))?;
    Ok(ReportRateInfo {
        current: rate_of(interval),
        supported: supported_rates(),
    })
}

/// Make the mouse report at `rate` until it next switches profile, and
/// return what it reports afterwards.
pub(crate) async fn set_report_rate(
    link: &G700Link,
    rate: ReportRate,
) -> Result<ReportRateInfo, WriteError> {
    let operation = HidppOperation::WriteReportRate;
    let Some(interval) = interval_of(rate) else {
        return Err(WriteError::ReportRateUnsupported { requested: rate });
    };
    let before = report_rate_info(link).await?;
    if before.current == rate {
        debug!(%rate, "G700 report rate already set");
        return Ok(before);
    }
    match set_report_interval_on(link.chan(), link.device_index(), interval).await {
        Ok(actual) => Ok(ReportRateInfo {
            current: rate_of(actual),
            ..before
        }),
        Err(G700Error::ReportIntervalNotApplied { actual, .. }) => {
            Err(WriteError::ReportRateNotApplied {
                requested: rate,
                actual: rate_of(actual),
            })
        }
        // The write was acknowledged; only its read-back failed.
        Err(G700Error::RegisterRead { source, .. }) => {
            debug!(?source, %rate, "wrote the G700 report interval (read-back failed)");
            Ok(ReportRateInfo {
                current: rate,
                ..before
            })
        }
        Err(error) => Err(g700_error(error, operation)),
    }
}

/// A register access that failed during `operation`.
fn register_error(
    source: hidpp::protocol::v10::Hidpp10Error,
    operation: HidppOperation,
) -> WriteError {
    use hidpp::{channel::ChannelError, protocol::v10::Hidpp10Error};
    match source {
        Hidpp10Error::Channel(ChannelError::NoResponse | ChannelError::Timeout) => {
            WriteError::RequestTimedOut { operation }
        }
        other => WriteError::Hidpp(format!("{operation:?}: {other:?}")),
    }
}

/// A [`G700Error`] of a write during `operation`.
fn g700_error(error: G700Error, operation: HidppOperation) -> WriteError {
    match error {
        G700Error::Device(error) => error,
        G700Error::RegisterRead { source, .. } | G700Error::RegisterWrite { source, .. } => {
            register_error(source, operation)
        }
        other => WriteError::Hidpp(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
