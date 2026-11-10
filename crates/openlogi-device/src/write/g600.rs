//! Driver for the Logitech G600 MMO gaming mouse.
//!
//! The G600 answers no HID++ request. Everything it is told lives in feature
//! reports on its vendor collection: report `0xf0` is the active profile and
//! DPI slot, `0xf3`–`0xf5` the three onboard profiles, each a 154-byte block
//! holding a DPI table, the report rate, the LED and forty button
//! assignments ([`report`] has the layout). Like the Litra driver it sits
//! beside the HID++ feature writers: the registry owns product matching and
//! the backend owns opening the node.
//!
//! This driver reads the profiles and selects the DPI slot through `0xf0`.
//! It never rewrites a profile: that block carries every button of the
//! mouse, so one wrong byte leaves the mouse with buttons that do nothing.

mod report;
#[cfg(test)]
mod tests;

use std::time::Duration;

use openlogi_core::hid::{Dpi, DpiCapabilities, DpiInfo};
use tracing::debug;

use crate::backend::{HidBackend, RawFeatureHandle};
use crate::channel::route::{DeviceRoute, find_raw_route_node};

use self::report::{
    ACTIVE_REPORT_ID, ActiveState, DpiSlot, G600ReportError, ProfileReport, ProfileSlot,
    encode_select_dpi_slot,
};
use super::raw_lock::device_lock;
use super::{HidppOperation, WriteError};

pub use self::report::{ACTIVE_REPORT_LEN, FEATURE_REPORT_LEN};
pub use openlogi_device_registry::g600::{G600_DRIVER_ID, matches_g600};

/// How long one feature-report exchange may take before the mouse counts as
/// not answering.
const FEATURE_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether `route` is the G600's vendor feature-report collection — the
/// raw-HID counterpart of [`super::litra_model_for_route`].
#[must_use]
pub fn is_g600_route(route: &DeviceRoute) -> bool {
    let DeviceRoute::RawHid {
        vendor_id,
        product_id,
        usage_page,
        usage_id,
        ..
    } = route
    else {
        return false;
    };
    matches_g600(*vendor_id, *product_id, *usage_page, *usage_id)
}

/// Every G600 report as the mouse holds it, for diagnostics and fixtures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G600Dump {
    /// Report `0xf0`: the active profile and DPI slot.
    pub active: [u8; ACTIVE_REPORT_LEN],
    /// Reports `0xf3`–`0xf5`: the three onboard profiles.
    pub profiles: [[u8; FEATURE_REPORT_LEN]; 3],
}

/// What one onboard profile sets, as far as this driver reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G600ProfileSummary {
    /// The four DPI slots; `None` is a slot the profile disables.
    pub dpi_slots: [Option<Dpi>; 4],
    /// The slot the profile starts in, 0-based; `None` when it names none.
    pub default_dpi_slot: Option<u8>,
    /// The DPI the DPI-shift button drops to while held; `None` when disabled.
    pub dpi_shift: Option<Dpi>,
    /// The USB report rate in hertz.
    pub report_rate_hz: u16,
    /// The LED colour as red, green, blue.
    pub led: [u8; 3],
}

/// The mouse's active selection and the three profiles it chooses from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct G600Status {
    /// The active profile, 0-based.
    pub active_profile: u8,
    /// The active DPI slot, 0-based, within the active profile.
    pub active_dpi_slot: u8,
    /// The three onboard profiles, in report order.
    pub profiles: [G600ProfileSummary; 3],
}

/// The DPI slots of the active profile and the one in use, in the shape the
/// HID++ DPI reads use.
///
/// The values are the active profile's enabled slots. The current value is
/// the active slot's; when the mouse's selection points at a slot the profile
/// disables, the resolution it moves at is not knowable from its reports, so
/// the profile's default slot stands in, or failing that the lowest slot.
pub async fn get_dpi_info(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<DpiInfo, WriteError> {
    let _guard = device_lock(route).await;
    let mut handle = connect(backend, route).await?;
    let (active, profile) = read_active_profile(handle.as_mut()).await?;
    dpi_info(active, &profile)
}

/// Select the active profile's slot holding exactly `dpi`.
///
/// Fails with [`WriteError::DpiUnsupported`], without writing, when no slot
/// of the active profile holds `dpi` — the G600 has no free-form resolution,
/// only its four slots — and with [`WriteError::DpiNotApplied`] when the
/// mouse still reports another slot after one re-select.
pub async fn set_dpi(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
    dpi: Dpi,
) -> Result<(), WriteError> {
    let _guard = device_lock(route).await;
    let mut handle = connect(backend, route).await?;
    let (active, profile) = read_active_profile(handle.as_mut()).await?;
    let Some(slot) = DpiSlot::ALL
        .into_iter()
        .find(|slot| profile.dpi_slot(*slot) == Some(dpi))
    else {
        return Err(WriteError::DpiUnsupported { requested: dpi });
    };
    let select = encode_select_dpi_slot(slot);
    write_report(handle.as_mut(), &select).await?;
    let mut after = read_active(handle.as_mut()).await?;
    if after.dpi_slot != slot {
        // A select the mouse acknowledged but did not apply is otherwise
        // invisible; one re-select tells a drowsy mouse from a refusal.
        debug!(route = %route, %dpi, "G600 reports another DPI slot after a select — selecting again");
        write_report(handle.as_mut(), &select).await?;
        after = read_active(handle.as_mut()).await?;
    }
    if after.dpi_slot == slot {
        debug!(route = %route, %dpi, profile = active.profile.index(), slot = slot.index(), "selected G600 DPI slot");
        return Ok(());
    }
    Err(WriteError::DpiNotApplied {
        requested: dpi,
        actual: current_dpi(after, &profile).unwrap_or(dpi),
    })
}

/// The mouse's active selection and what each of its profiles sets.
pub async fn read_status(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<G600Status, WriteError> {
    let _guard = device_lock(route).await;
    let mut handle = connect(backend, route).await?;
    let active = read_active(handle.as_mut()).await?;
    let profiles = read_profiles(handle.as_mut()).await?;
    Ok(G600Status {
        active_profile: active.profile.index(),
        active_dpi_slot: active.dpi_slot.index(),
        profiles: profiles.map(|profile| G600ProfileSummary {
            dpi_slots: profile.dpi_slots(),
            default_dpi_slot: profile.default_dpi_slot().map(DpiSlot::index),
            dpi_shift: profile.dpi_shift(),
            report_rate_hz: profile.report_rate_hz(),
            led: profile.led(),
        }),
    })
}

/// Every report as the mouse holds it.
pub async fn dump_reports(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<G600Dump, WriteError> {
    let _guard = device_lock(route).await;
    let mut handle = connect(backend, route).await?;
    let report = read_report(handle.as_mut(), ACTIVE_REPORT_ID, HidppOperation::ReadDpi).await?;
    let active: [u8; ACTIVE_REPORT_LEN] = report
        .get(..ACTIVE_REPORT_LEN)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| {
            malformed(G600ReportError::Length {
                report_id: report.first().copied(),
                len: report.len(),
            })
        })?;
    let profiles = read_profiles(handle.as_mut()).await?;
    Ok(G600Dump {
        active,
        profiles: profiles.map(ProfileReport::into_bytes),
    })
}

fn dpi_info(active: ActiveState, profile: &ProfileReport) -> Result<DpiInfo, WriteError> {
    let values = profile
        .dpi_slots()
        .into_iter()
        .flatten()
        .map(Dpi::into_inner)
        .collect();
    let capabilities = DpiCapabilities::new(values)?;
    let current = current_dpi(active, profile).unwrap_or_else(|| capabilities.min());
    Ok(DpiInfo {
        current,
        capabilities,
    })
}

/// The DPI the mouse moves at, as far as its reports say: the active slot's
/// value, or the default slot's when the active slot is disabled.
fn current_dpi(active: ActiveState, profile: &ProfileReport) -> Option<Dpi> {
    profile.dpi_slot(active.dpi_slot).or_else(|| {
        profile
            .default_dpi_slot()
            .and_then(|slot| profile.dpi_slot(slot))
    })
}

/// Open the G600 `route` addresses for feature reports.
///
/// A route that is not the G600's vendor collection is [`WriteError::DeviceNotFound`]
/// before any node is looked up: the route's tuple would otherwise find
/// whatever device it does name, and that device would be sent G600 reports.
async fn connect(
    backend: &dyn HidBackend,
    route: &DeviceRoute,
) -> Result<Box<dyn RawFeatureHandle>, WriteError> {
    if !is_g600_route(route) {
        return Err(WriteError::DeviceNotFound);
    }
    let Some(node) = find_raw_route_node(backend, route).await? else {
        return Err(WriteError::DeviceNotFound);
    };
    Ok(backend.open_raw_feature(&node).await?)
}

async fn read_active_profile(
    handle: &mut dyn RawFeatureHandle,
) -> Result<(ActiveState, ProfileReport), WriteError> {
    let active = read_active(handle).await?;
    let profile = read_profile(handle, active.profile).await?;
    Ok((active, profile))
}

async fn read_active(handle: &mut dyn RawFeatureHandle) -> Result<ActiveState, WriteError> {
    let report = read_report(handle, ACTIVE_REPORT_ID, HidppOperation::ReadDpi).await?;
    ActiveState::decode(&report).map_err(malformed)
}

async fn read_profile(
    handle: &mut dyn RawFeatureHandle,
    slot: ProfileSlot,
) -> Result<ProfileReport, WriteError> {
    let report = read_report(
        handle,
        slot.report_id(),
        HidppOperation::ReadDpiCapabilities,
    )
    .await?;
    ProfileReport::decode(&report).map_err(malformed)
}

async fn read_profiles(
    handle: &mut dyn RawFeatureHandle,
) -> Result<[ProfileReport; 3], WriteError> {
    let [first, second, third] = ProfileSlot::ALL;
    Ok([
        read_profile(handle, first).await?,
        read_profile(handle, second).await?,
        read_profile(handle, third).await?,
    ])
}

/// Read the feature report `report_id`, in a buffer the host may fill to the
/// collection's declared length, trimmed to what it handed back.
async fn read_report(
    handle: &mut dyn RawFeatureHandle,
    report_id: u8,
    operation: HidppOperation,
) -> Result<Vec<u8>, WriteError> {
    let mut report = vec![0; FEATURE_REPORT_LEN];
    report[0] = report_id;
    let len = tokio::time::timeout(FEATURE_IO_TIMEOUT, handle.read_feature_report(&mut report))
        .await
        .map_err(|_elapsed| WriteError::RequestTimedOut { operation })??;
    report.truncate(len);
    Ok(report)
}

async fn write_report(handle: &mut dyn RawFeatureHandle, report: &[u8]) -> Result<(), WriteError> {
    tokio::time::timeout(FEATURE_IO_TIMEOUT, handle.write_feature_report(report))
        .await
        .map_err(|_elapsed| WriteError::RequestTimedOut {
            operation: HidppOperation::WriteDpi,
        })??;
    Ok(())
}

fn malformed(error: G600ReportError) -> WriteError {
    WriteError::Hid(format!("the G600 answered a malformed report: {error}"))
}
